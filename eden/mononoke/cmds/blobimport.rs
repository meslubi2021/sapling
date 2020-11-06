/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use anyhow::{bail, format_err, Context, Error, Result};
use ascii::AsciiString;
use blobimport_lib;
use blobrepo::BlobRepo;
use bonsai_globalrev_mapping::SqlBonsaiGlobalrevMapping;
use clap::{App, Arg, ArgMatches};
use cmdlib::{
    args,
    helpers::{block_execute, upload_and_show_trace},
};
use context::CoreContext;
use derived_data::BonsaiDerived;
use derived_data_filenodes::FilenodesOnlyPublic;
use derived_data_utils::POSSIBLE_DERIVED_TYPES;
use failure_ext::SlogKVError;
use fbinit::FacebookInit;
use futures::{
    compat::Future01CompatExt,
    future::{try_join, try_join4, FutureExt, TryFutureExt},
};
#[cfg(fbcode_build)]
use mercurial_revlog::revlog::RevIdx;
use mercurial_types::{HgChangesetId, HgNodeHash};
use mononoke_types::ChangesetId;
use mutable_counters::MutableCounters;
use mutable_counters::SqlMutableCounters;
use slog::{error, info, warn, Logger};
use std::collections::HashMap;
use std::fs::read;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use synced_commit_mapping::SqlSyncedCommitMapping;

const ARG_DERIVED_DATA_TYPE: &str = "derived-data-type";
const ARG_FIND_ALREADY_IMPORTED_REV_ONLY: &str = "find-already-imported-rev-only";

fn setup_app<'a, 'b>() -> App<'a, 'b> {
    args::MononokeApp::new("revlog to blob importer")
        .with_repo_required()
        .with_source_repos()
        .build()
        .about("Import a revlog-backed Mercurial repo into Mononoke blobstore.")
        .args_from_usage(
            r#"
            <INPUT>                          'input revlog repo'
            --changeset [HASH]               'if provided, the only changeset to be imported'
            --no-bookmark                    'if provided won't update bookmarks'
            --prefix-bookmark [PREFIX]       'if provided will update bookmarks, but prefix them with PREFIX'
            --no-create                      'if provided won't create a new repo (only meaningful for local)'
            --lfs-helper [LFS_HELPER]        'if provided, path to an executable that accepts OID SIZE and returns a LFS blob to stdout'
            --concurrent-changesets [LIMIT]  'if provided, max number of changesets to upload concurrently'
            --concurrent-blobs [LIMIT]       'if provided, max number of blobs to process concurrently'
            --concurrent-lfs-imports [LIMIT] 'if provided, max number of LFS files to import concurrently'
            --has-globalrev                  'if provided will update globalrev'
            --manifold-next-rev-to-import [KEY] 'if provided then this manifold key will be updated with the next revision to import'
            --manifold-bucket [BUCKET]        'can only be used if --manifold-next-rev-to-import is set'
        "#,
        )
        .arg(
            Arg::from_usage("--skip [SKIP]  'skips commits from the beginning'")
                .conflicts_with("changeset"),
        )
        .arg(
            Arg::from_usage(
                "--commits-limit [LIMIT] 'import only LIMIT first commits from revlog repo'",
            )
            .conflicts_with("changeset"),
        )
        .arg(
            Arg::with_name("fix-parent-order")
                .long("fix-parent-order")
                .value_name("FILE")
                .takes_value(true)
                .required(false)
                .help(
                    "file which fixes order or parents for commits in format 'HG_CS_ID P1_CS_ID [P2_CS_ID]'\
                     This is useful in case of merge commits - mercurial ignores order of parents of the merge commit \
                     while Mononoke doesn't ignore it. That might result in different bonsai hashes for the same \
                     Mercurial commit. Using --fix-parent-order allows to fix order of the parents."
                 )
        )
        .arg(
            Arg::with_name(ARG_DERIVED_DATA_TYPE)
                .long(ARG_DERIVED_DATA_TYPE)
                .takes_value(true)
                .multiple(true)
                .required(false)
                .possible_values(POSSIBLE_DERIVED_TYPES)
                .help("Derived data type to be backfilled. Note - 'filenodes' will always be derived")
        )
        .arg(
            Arg::with_name(ARG_FIND_ALREADY_IMPORTED_REV_ONLY)
                .long(ARG_FIND_ALREADY_IMPORTED_REV_ONLY)
                .takes_value(false)
                .multiple(false)
                .required(false)
                .help("Does not do any import. Just finds the rev that was already imported rev and \
                      updates manifold-next-rev-to-import if it's set. Note that we might have \
                      a situation where revision i is imported, i+1 is not and i+2 is imported. \
                      In that case this function would return i.")
        )
}

fn parse_fixed_parent_order<P: AsRef<Path>>(
    logger: &Logger,
    p: P,
) -> Result<HashMap<HgChangesetId, Vec<HgChangesetId>>> {
    let content = read(p)?;
    let mut res = HashMap::new();

    for line in String::from_utf8(content).map_err(Error::from)?.split("\n") {
        if line.is_empty() {
            continue;
        }
        let mut iter = line.split(" ").map(HgChangesetId::from_str).fuse();
        let maybe_hg_cs_id = iter.next();
        let hg_cs_id = match maybe_hg_cs_id {
            Some(hg_cs_id) => hg_cs_id?,
            None => {
                continue;
            }
        };

        let parents = match (iter.next(), iter.next()) {
            (Some(p1), Some(p2)) => vec![p1?, p2?],
            (Some(p), None) => {
                warn!(
                    logger,
                    "{}: parent order is fixed for a single parent, most likely won't have any effect",
                    hg_cs_id,
                );
                vec![p?]
            }
            (None, None) => {
                warn!(
                    logger,
                    "{}: parent order is fixed for a commit with no parents, most likely won't have any effect",
                    hg_cs_id,
                );
                vec![]
            }
            (None, Some(_)) => unreachable!(),
        };
        if let Some(_) = iter.next() {
            bail!("got 3 parents, but mercurial supports at most 2!");
        }

        if res.insert(hg_cs_id, parents).is_some() {
            warn!(logger, "order is fixed twice for {}!", hg_cs_id);
        }
    }
    Ok(res)
}

#[cfg(fbcode_build)]
async fn update_manifold_key(
    fb: FacebookInit,
    latest_imported_rev: RevIdx,
    manifold_key: String,
    manifold_bucket: String,
) -> Result<()> {
    use bytes::Bytes;
    use manifold::{ObjectMeta, PayloadDesc, StoredObject};
    use manifold_thrift::thrift::{self, manifold_thrift_new, RequestContext};

    let next_revision_to_import = latest_imported_rev.as_u32() + 1;
    let context = RequestContext {
        bucketName: manifold_bucket,
        apiKey: "".to_string(),
        timeoutMsec: 10000,
        ..Default::default()
    };
    let object_meta = ObjectMeta {
        ..Default::default()
    };
    let bytes = Bytes::from(format!("{}", next_revision_to_import));
    let object = thrift::StoredObject::from(StoredObject {
        meta: object_meta,
        payload: PayloadDesc::from(bytes),
    });

    let client = manifold_thrift_new(fb)?;
    thrift::write_chunked(&client, &context, &manifold_key, &object).await
}

async fn run_blobimport<'a>(
    fb: FacebookInit,
    ctx: &CoreContext,
    logger: &Logger,
    matches: &'a ArgMatches<'a>,
) -> Result<()> {
    let config_store = args::init_config_store(fb, logger, matches)?;

    let revlogrepo_path = matches
        .value_of("INPUT")
        .expect("input is not specified")
        .into();

    let changeset = match matches.value_of("changeset") {
        None => None,
        Some(hash) => Some(HgNodeHash::from_str(hash)?),
    };

    let skip = if !matches.is_present("skip") {
        None
    } else {
        Some(args::get_usize(&matches, "skip", 0))
    };

    let commits_limit = if !matches.is_present("commits-limit") {
        None
    } else {
        Some(args::get_usize(&matches, "commits-limit", 0))
    };

    let manifold_key = matches
        .value_of("manifold-next-rev-to-import")
        .map(|s| s.to_string());

    let manifold_bucket = matches.value_of("manifold-bucket").map(|s| s.to_string());

    let manifold_key_bucket = match (manifold_key, manifold_bucket) {
        (Some(key), Some(bucket)) => Some((key, bucket)),
        (None, None) => None,
        _ => {
            return Err(format_err!(
                "invalid manifold parameters: bucket and key should either both be specified or none"
            ));
        }
    };

    let no_bookmark = matches.is_present("no-bookmark");
    let prefix_bookmark = matches.value_of("prefix-bookmark");
    if no_bookmark && prefix_bookmark.is_some() {
        return Err(format_err!(
            "--no-bookmark is incompatible with --prefix-bookmark"
        ));
    }

    let bookmark_import_policy = if no_bookmark {
        blobimport_lib::BookmarkImportPolicy::Ignore
    } else {
        let prefix = match prefix_bookmark {
            Some(prefix) => AsciiString::from_ascii(prefix).unwrap(),
            None => AsciiString::new(),
        };
        blobimport_lib::BookmarkImportPolicy::Prefix(prefix)
    };

    let lfs_helper = matches.value_of("lfs-helper").map(|l| l.to_string());

    let concurrent_changesets = args::get_usize(&matches, "concurrent-changesets", 100);
    let concurrent_blobs = args::get_usize(&matches, "concurrent-blobs", 100);
    let concurrent_lfs_imports = args::get_usize(&matches, "concurrent-lfs-imports", 10);

    let fixed_parent_order = if let Some(path) = matches.value_of("fix-parent-order") {
        parse_fixed_parent_order(&logger, path)
            .context("while parsing file with fixed parent order")?
    } else {
        HashMap::new()
    };

    let mut derived_data_types = matches
        .values_of(ARG_DERIVED_DATA_TYPE)
        .map(|v| v.map(|d| d.to_string()).collect())
        .unwrap_or(vec![]);

    // Filenodes will be unconditionally derived, since blobimport imports public
    // hg changesets which must have filenodes derived
    let filenodes_derived_name = FilenodesOnlyPublic::NAME.to_string();
    if !derived_data_types.contains(&filenodes_derived_name) {
        derived_data_types.push(filenodes_derived_name);
    }

    let has_globalrev = matches.is_present("has-globalrev");

    let (_repo_name, repo_config) = args::get_config(config_store, &matches)?;
    let populate_git_mapping = repo_config.pushrebase.populate_git_mapping.clone();

    let small_repo_id = args::get_source_repo_id_opt(config_store, &matches)?;

    let (blobrepo, globalrevs_store, synced_commit_mapping, mutable_counters) = try_join4(
        async {
            if matches.is_present("no-create") {
                args::open_repo_unredacted(fb, &ctx.logger(), &matches).await
            } else {
                args::create_repo_unredacted(fb, &ctx.logger(), &matches).await
            }
        },
        args::open_sql::<SqlBonsaiGlobalrevMapping>(fb, config_store, &matches),
        args::open_sql::<SqlSyncedCommitMapping>(fb, config_store, &matches),
        args::open_sql::<SqlMutableCounters>(fb, config_store, &matches),
    )
    .await?;

    let globalrevs_store = Arc::new(globalrevs_store);
    let synced_commit_mapping = Arc::new(synced_commit_mapping);

    let find_latest_imported_rev_only = matches.is_present(ARG_FIND_ALREADY_IMPORTED_REV_ONLY);
    async move {
        let blobimport = blobimport_lib::Blobimport {
            ctx,
            blobrepo: blobrepo.clone(),
            revlogrepo_path,
            changeset,
            skip,
            commits_limit,
            bookmark_import_policy,
            globalrevs_store,
            synced_commit_mapping,
            lfs_helper,
            concurrent_changesets,
            concurrent_blobs,
            concurrent_lfs_imports,
            fixed_parent_order,
            has_globalrev,
            populate_git_mapping,
            small_repo_id,
            derived_data_types,
        };

        let maybe_latest_imported_rev = if find_latest_imported_rev_only {
            blobimport.find_already_imported_revision().await?
        } else {
            blobimport.import().await?
        };

        match maybe_latest_imported_rev {
            Some((latest_imported_rev, latest_imported_cs_id)) => {
                info!(
                    ctx.logger(),
                    "latest imported revision {}",
                    latest_imported_rev.as_u32()
                );
                #[cfg(fbcode_build)]
                {
                    if let Some((manifold_key, bucket)) = manifold_key_bucket {
                        update_manifold_key(fb, latest_imported_rev, manifold_key, bucket).await?
                    }
                }
                #[cfg(not(fbcode_build))]
                {
                    assert!(
                        manifold_key_bucket.is_none(),
                        "Using Manifold is not supported in non fbcode builds"
                    );
                }

                maybe_update_highest_imported_generation_number(
                    &ctx,
                    &blobrepo,
                    &mutable_counters,
                    latest_imported_cs_id,
                )
                .await?;
            }
            None => info!(ctx.logger(), "didn't import any commits"),
        };
        upload_and_show_trace(ctx.clone())
            .compat()
            .map(|_| ())
            .await;

        Ok(())
    }
    .map_err({
        move |err| {
            // NOTE: We log the error immediatley, then provide another one for main's
            // Result (which will set our exit code).
            error!(ctx.logger(), "error while blobimporting"; SlogKVError(err));
            Error::msg("blobimport exited with a failure")
        }
    })
    .await
}

// Updating mutable_counters table to store the highest generation number that was imported.
// This in turn can be used to track which commits exist on both mercurial and Mononoke.
// For example, WarmBookmarkCache might consider a bookmark "warm" only if a commit is in both
// mercurial and Mononoke.
//
// Note that if a commit with a lower generation number was added (e.g. if this commit forked off from
// the main branch) then this hint will be misleading - i.e. the hint would store a higher generation
// number then the new commit which might not be processed by blobimport yet. In that case there are
// two options:
// 1) Use this hint only in single-branch repos
// 2) Accept that the hint might be incorrect sometimes.
async fn maybe_update_highest_imported_generation_number(
    ctx: &CoreContext,
    blobrepo: &BlobRepo,
    mutable_counters: &SqlMutableCounters,
    latest_imported_cs_id: ChangesetId,
) -> Result<(), Error> {
    let maybe_highest_imported_gen_num = mutable_counters
        .get_counter(
            ctx.clone(),
            blobrepo.get_repoid(),
            blobimport_lib::HIGHEST_IMPORTED_GEN_NUM,
        )
        .compat();
    let new_gen_num = blobrepo
        .get_generation_number(ctx.clone(), latest_imported_cs_id)
        .compat();
    let (maybe_highest_imported_gen_num, new_gen_num) =
        try_join(maybe_highest_imported_gen_num, new_gen_num).await?;

    let new_gen_num = new_gen_num.ok_or(format_err!("generation number is not set"))?;
    let new_gen_num = match maybe_highest_imported_gen_num {
        Some(highest_imported_gen_num) => {
            if new_gen_num.value() as i64 > highest_imported_gen_num {
                Some(new_gen_num)
            } else {
                None
            }
        }
        None => Some(new_gen_num),
    };

    if let Some(new_gen_num) = new_gen_num {
        mutable_counters
            .set_counter(
                ctx.clone(),
                blobrepo.get_repoid(),
                blobimport_lib::HIGHEST_IMPORTED_GEN_NUM,
                new_gen_num.value() as i64,
                maybe_highest_imported_gen_num,
            )
            .compat()
            .await?;
    }
    Ok(())
}

#[fbinit::main]
fn main(fb: FacebookInit) -> Result<()> {
    let matches = setup_app().get_matches();

    args::init_cachelib(fb, &matches, None);
    let logger = args::init_logging(fb, &matches);
    let ctx = &CoreContext::new_with_logger(fb, logger.clone());
    args::init_config_store(fb, &logger, &matches)?;

    block_execute(
        run_blobimport(fb, ctx, &logger, &matches),
        fb,
        "blobimport",
        &logger,
        &matches,
        cmdlib::monitoring::AliveService,
    )
}
