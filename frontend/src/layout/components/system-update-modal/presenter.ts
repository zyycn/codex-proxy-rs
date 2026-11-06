import type { SystemUpdateDetail, SystemVersion } from '@/api'
import { ArrowUpCircle, CheckCircle2, RefreshCw, TriangleAlert, XCircle } from '@lucide/vue'
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
}

export function resolveSystemUpdatePresentation(input: SystemUpdatePresentationInput) {
  return {
    status: resolveStatus(input),
    summaryItems: resolveSummaryItems(input),
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
      return { marker: 'text-z-success', text: 'text-z-success' }
    case 'warning':
      return { marker: 'text-z-warning', text: 'text-z-warning' }
    case 'error':
      return { marker: 'text-z-error', text: 'text-z-error' }
    default:
      return { marker: 'text-z-info', text: 'text-z-text' }
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
      badge: 'bg-z-info-container text-z-info-on-container',
      iconClass: 'text-z-info',
    }
  }
  if (input.updateError) {
    return {
      label: t('common.status.error'),
      icon: XCircle,
      badge: 'bg-z-error-container text-z-error-on-container',
      iconClass: 'text-z-error',
    }
  }
  if (input.updateInfo?.warning && !input.needRestart) {
    return {
      label: t('systemUpdate.status.warning'),
      icon: TriangleAlert,
      badge: 'bg-z-warning-container text-z-warning-on-container',
      iconClass: 'text-z-warning-on-container',
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
      badge: 'bg-z-success-container text-z-success-on-container',
      iconClass: 'text-z-success',
    }
  }
  return {
    label: t('systemUpdate.status.notChecked'),
    icon: CheckCircle2,
    badge: 'bg-z-fill-tertiary text-z-text-secondary',
    iconClass: 'text-z-text-quaternary',
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
      value: input.updateInfo?.warning
        ? t('systemUpdate.status.unconfirmedVersion')
        : versionLabel(input.updateInfo?.latestVersion),
      title: input.updateInfo?.warning ? undefined : input.updateInfo?.latestVersion,
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
