/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import {Tooltip} from '../Tooltip';
import {t} from '../i18n';
import {debugToolsEnabledState} from './DebugToolsState';
import {VSCodeButton} from '@vscode/webview-ui-toolkit/react';
import {lazy, Suspense} from 'react';
import {useRecoilValue} from 'recoil';
import {Icon} from 'shared/Icon';

const DebugToolsMenu = lazy(() => import('./DebugToolsMenu'));

export function DebugToolsButton() {
  const debugEnabled = useRecoilValue(debugToolsEnabledState);
  if (!debugEnabled) {
    return null;
  }
  return (
    <Tooltip
      component={() => (
        <Suspense fallback={<Icon icon="loading" />}>
          <DebugToolsMenu />
        </Suspense>
      )}
      title={t('Debug Tools')}
      trigger="click"
      placement="bottom">
      <VSCodeButton appearance="icon">
        <Icon icon="pulse" />
      </VSCodeButton>
    </Tooltip>
  );
}
