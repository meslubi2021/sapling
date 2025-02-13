/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Result;
use futures::stream::BoxStream;
use gix_hash::ObjectId;
use mononoke_types::ChangesetId;
use packfile::pack::DeltaForm;
use packfile::types::PackfileItem;

/// The set of refs that are to be included in or excluded from the pack
#[derive(Debug, Clone)]
pub enum RequestedRefs {
    /// Include the following refs with values known by the server
    Included(HashSet<String>),
    /// Include the following refs with values provided by the caller
    IncludedWithValue(HashMap<String, ChangesetId>),
    /// Exclude the following refs
    Excluded(HashSet<String>),
}

impl RequestedRefs {
    pub fn all() -> Self {
        RequestedRefs::Excluded(HashSet::new())
    }

    pub fn none() -> Self {
        RequestedRefs::Included(HashSet::new())
    }
}

/// Enum defining how annotated tags should be included as a ref
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagInclusion {
    // Peel the tag and map it to the underlying Git commit
    Peeled,
    // Include the tag as-is without peeling and map it to
    // the annotated Git tag object
    AsIs,
}

/// Enum defining whether a delta should be included in the pack
/// and if so, what kind of delta should be used
#[derive(Debug, Clone, Copy)]
pub enum DeltaInclusion {
    /// Include deltas with the provided form and inclusion threshold
    Include {
        /// Whether the pack input should consist of RefDeltas or only OffsetDeltas
        form: DeltaForm,
        /// The percentage threshold which should be satisfied by the delta to be included
        /// in the pack input stream. The threshold is expressed as percentage of the original (0.0 to 1.0)
        /// uncompressed object size. e.g. If original object size is 100 bytes and the
        /// delta_inclusion_threshold is 0.5, then the delta size should be less than 50 bytes
        inclusion_threshold: f32,
    },
    /// Do not include deltas
    Exclude,
}

/// The request parameters used to specify the constraints that need to be
/// honored while generating the input PackfileItem stream
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PackItemStreamRequest {
    /// The refs that are requested to be included/excluded from the pack
    pub requested_refs: RequestedRefs,
    /// The heads of the references that are present with the client
    pub have_heads: Vec<ChangesetId>,
    /// The type of delta that should be included in the pack, if any
    pub delta_inclusion: DeltaInclusion,
    /// How annotated tags should be included in the pack
    pub tag_inclusion: TagInclusion,
}

impl PackItemStreamRequest {
    pub fn new(
        requested_refs: RequestedRefs,
        have_heads: Vec<ChangesetId>,
        delta_inclusion: DeltaInclusion,
        tag_inclusion: TagInclusion,
    ) -> Self {
        Self {
            requested_refs,
            have_heads,
            delta_inclusion,
            tag_inclusion,
        }
    }

    pub fn full_repo(delta_inclusion: DeltaInclusion, tag_inclusion: TagInclusion) -> Self {
        Self {
            requested_refs: RequestedRefs::Excluded(HashSet::new()),
            have_heads: vec![],
            delta_inclusion,
            tag_inclusion,
        }
    }
}

/// Struct representing the packfile item response generated for the
/// given range of commits
pub struct PackItemStreamResponse<'a> {
    /// The stream of packfile items that were generated for the given range of commits
    pub items: BoxStream<'a, Result<PackfileItem>>,
    /// The number of packfile items that were generated for the given range of commits
    pub num_items: usize,
    /// The set of refs mapped to their Git commit ID or tag ID that are included in the
    /// generated stream of packfile items
    pub included_refs: HashMap<String, ObjectId>,
}

impl<'a> PackItemStreamResponse<'a> {
    pub fn new(
        items: BoxStream<'a, Result<PackfileItem>>,
        num_items: usize,
        included_refs: HashMap<String, ObjectId>,
    ) -> Self {
        Self {
            items,
            num_items,
            included_refs,
        }
    }
}
