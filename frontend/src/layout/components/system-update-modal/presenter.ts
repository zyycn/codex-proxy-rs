import type { SystemUpdateDetail, SystemVersion } from '@/api'
import { ArrowUpCircle, CheckCircle2, RefreshCw, XCircle } from '@lucide/vue'
import { t } from '@/i18n'

interface SystemUpdatePresentationInput {
  version: SystemVersion | null
  updateInfo: SystemUpdateDetail | null
  loading: boolean
  checking: boolean
  restarting: boolean
  updating: boolean
  updateError: string
  needRestart: boolean
  hasUpdate: boolean
  updateStreaming: boolean
  updateStreamError: string
  previousTargetVersion: string
  confirmedTargetVersion: string | null
}

export function resolveSystemUpdatePresentation(input: SystemUpdatePresentationInput) {
  return {
    status: resolveStatus(input),
    summaryItems: resolveSummaryItems(input),
    confirmRows: resolveConfirmRows(input),
    releaseVersion: displayValue(input.updateInfo?.latestVersion),
    streamStatusLabel: input.updateStreaming
      ? t('systemUpdate.logs.live')
      : input.updateStreamError
        ? t('systemUpdate.logs.disconnectedState')
        : t('systemUpdate.logs.waiting'),
    restartButtonLabel: input.restarting
      ? t('systemUpdate.status.restarting')
      : t('systemUpdate.actions.restart'),
  }
}

export function resolveSystemUpdateLogClasses(level: string) {
  switch (level) {
    case 'success':
      return { marker: 'text-cp-success', text: 'text-cp-success' }
    case 'warning':
      return { marker: 'text-cp-warning', text: 'text-cp-warning' }
    case 'error':
      return { marker: 'text-cp-error', text: 'text-cp-error' }
    default:
      return { marker: 'text-cp-info', text: 'text-cp-text' }
  }
}

function resolveStatus(input: SystemUpdatePresentationInput) {
  if (input.restarting || input.updating || input.checking || input.loading) {
    return {
      label: input.restarting
        ? t('systemUpdate.status.restarting')
        : input.updating
          ? t('systemUpdate.status.updating')
          : t('systemUpdate.status.checking'),
      icon: RefreshCw,
      badge: 'bg-cp-info-container text-cp-info-on-container',
      iconClass: 'text-cp-info',
    }
  }
  if (input.updateError || input.updateInfo?.warning) {
    return {
      label: t('common.status.error'),
      icon: XCircle,
      badge: 'bg-cp-error-container text-cp-error-on-container',
      iconClass: 'text-cp-error',
    }
  }
  if (input.needRestart || input.hasUpdate || input.updateInfo) {
    return {
      label: input.needRestart
        ? t('systemUpdate.status.restartRequired')
        : input.hasUpdate
          ? t('systemUpdate.status.available')
          : t('systemUpdate.status.upToDate'),
      icon: input.hasUpdate ? ArrowUpCircle : CheckCircle2,
      badge: 'bg-cp-success-container text-cp-success-on-container',
      iconClass: 'text-cp-success',
    }
  }
  return {
    label: t('systemUpdate.status.notChecked'),
    icon: CheckCircle2,
    badge: 'bg-cp-fill-tertiary text-cp-text-secondary',
    iconClass: 'text-cp-text-quaternary',
  }
}

function resolveSummaryItems(input: SystemUpdatePresentationInput) {
  return [
    {
      key: 'current',
      label: t('systemUpdate.currentVersion'),
      value: input.loading
        ? '...'
        : versionLabel(input.updateInfo?.currentVersion ?? input.version?.version),
      title: input.updateInfo?.currentVersion ?? input.version?.version,
    },
    {
      key: 'latest',
      label: t('systemUpdate.latestVersion'),
      value: versionLabel(input.updateInfo?.latestVersion),
      title: input.updateInfo?.latestVersion,
      releaseUrl: input.updateInfo?.releaseUrl,
    },
    {
      key: 'build',
      label: t('systemUpdate.build'),
      value: buildTypeLabel(input.updateInfo?.buildType),
      title: input.updateInfo?.buildType,
    },
    {
      key: 'deployment',
      label: t('systemUpdate.deployment'),
      value: deploymentModeLabel(input.version?.deploymentMode),
      title: input.version?.deploymentMode,
    },
  ]
}

function resolveConfirmRows(input: SystemUpdatePresentationInput) {
  return [
    {
      key: 'current',
      label: t('systemUpdate.currentVersion'),
      value: versionLabel(input.version?.version),
    },
    {
      key: 'previous',
      label: t('systemUpdate.displayedTarget'),
      value: versionLabel(input.previousTargetVersion),
    },
    {
      key: 'target',
      label: t('systemUpdate.remoteTarget'),
      value: versionLabel(input.confirmedTargetVersion),
    },
  ]
}

function buildTypeLabel(value?: string) {
  switch (value) {
    case 'release': return t('systemUpdate.buildTypes.release')
    case 'source': return t('systemUpdate.buildTypes.source')
    case 'dev': return t('systemUpdate.buildTypes.dev')
    case 'experimental': return t('systemUpdate.buildTypes.experimental')
    default: return displayValue(value)
  }
}

function deploymentModeLabel(value?: string) {
  switch (value) {
    case 'docker': return t('systemUpdate.deploymentModes.docker')
    case 'binary': return t('systemUpdate.deploymentModes.binary')
    case 'source': return t('systemUpdate.deploymentModes.source')
    default: return displayValue(value)
  }
}

function versionLabel(value?: string | null) {
  return value
    ? `v${value}`
    : '-'
}

function displayValue(value: unknown) {
  return value
    ? String(value)
    : '-'
}
