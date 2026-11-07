<script setup lang="ts">
import type { SystemRestartPlan, SystemUpdateChannel } from '@/api'

import { ZButton, ZConfirmDialog, ZDialog, ZMarkdown, ZPopover, ZScrollbar, ZSegmented, ZSkeleton } from '@codex-proxy/ui'
import {
  ArrowUpCircle,
  Circle,
  CircleHelp,
  Download,
  ExternalLink,
  History,
  Hourglass,
  Power,
  RefreshCw,
  Terminal,
  TriangleAlert,
} from '@lucide/vue'
import { storeToRefs } from 'pinia'

import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { checkSystemRestart } from '@/api'
import { useSystemUpdateStore } from '@/stores/modules/system-update'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import {
  resolveSystemUpdateLogClasses,
  resolveSystemUpdatePresentation,
} from './presenter'

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })

const systemUpdateStore = useSystemUpdateStore()
const {
  version,
  updateInfo,
  loading,
  checking,
  changingChannel,
  selectedChannel,
  availableChannels,
  canChangeChannel,
  updating,
  restarting,
  updateError,
  lastFailedOperation,
  needRestart,
  updateLogs,
  updateStreaming,
  updateStreamError,
  hasCandidateUpdate: hasUpdate,
  canUpdate,
  canRestart,
} = storeToRefs(systemUpdateStore)
const { loadSystem, checkUpdates, changeChannel, updateNow, restartNow } = systemUpdateStore

const updateLogScrollbar = useTemplateRef<InstanceType<typeof ZScrollbar>>('updateLogScrollbar')
const checkingRestart = shallowRef(false)
const restartConfirmOpen = shallowRef(false)
const restartPlan = shallowRef<SystemRestartPlan | null>(null)

const presentation = computed(() => resolveSystemUpdatePresentation({
  version: version.value,
  updateInfo: updateInfo.value,
  loading: loading.value,
  checking: checking.value || changingChannel.value,
  restarting: restarting.value,
  updating: updating.value,
  updateError: updateError.value,
  needRestart: needRestart.value,
  hasUpdate: hasUpdate.value,
  updateStreaming: updateStreaming.value,
  updateStreamError: updateStreamError.value,
}))

const updateLogRows = computed(() =>
  updateLogs.value.map(item => ({
    ...item,
    time: item.atDisplay || '--:--:--',
    classes: resolveSystemUpdateLogClasses(item.level),
  })),
)

const channelLabels = computed(() => ({
  stable: {
    label: t('systemUpdate.channels.stable.label'),
    description: t('systemUpdate.channels.stable.description'),
  },
  rc: {
    label: t('systemUpdate.channels.rc.label'),
    description: t('systemUpdate.channels.rc.description'),
  },
  beta: {
    label: t('systemUpdate.channels.beta.label'),
    description: t('systemUpdate.channels.beta.description'),
  },
  alpha: {
    label: t('systemUpdate.channels.alpha.label'),
    description: t('systemUpdate.channels.alpha.description'),
  },
  exp: {
    label: t('systemUpdate.channels.experimental.label'),
    description: t('systemUpdate.channels.experimental.description'),
  },
}))
const channelOptions = computed(() => availableChannels.value.map(value => ({
  value,
  label: {
    stable: 'Stable',
    rc: 'RC',
    beta: 'Beta',
    alpha: 'Alpha',
    exp: 'Exp',
  }[value],
})))

async function handleChannelChange(value: string) {
  if (!availableChannels.value.includes(value as SystemUpdateChannel))
    return
  try {
    await changeChannel(value as SystemUpdateChannel)
  }
  catch {}
}

const hasReleaseNotes = computed(() => Boolean(updateInfo.value?.notes?.trim()))
const releaseNotesLoading = computed(() => loading.value || changingChannel.value)

const showUpdateProgress = computed(
  () => updating.value || restarting.value || updateLogRows.value.length > 0,
)

async function scrollUpdateLogsToBottom() {
  await nextTick()
  await updateLogScrollbar.value?.scrollToBottom()
}

function pinUpdateLogsToBottom() {
  window.requestAnimationFrame(() => {
    void scrollUpdateLogsToBottom()
  })
}

async function handleCheckUpdates(force = true) {
  try {
    const data = await checkUpdates(force)
    if (!data || data.warning)
      return
    ZNotification.success({
      message: data.hasUpdate
        ? t('systemUpdate.feedback.available')
        : t('systemUpdate.feedback.noUpdates'),
    })
  }
  catch {}
}

async function handleUpdateRequest() {
  const detail = updateInfo.value
  if (!canUpdate.value || !detail)
    return
  try {
    const result = await updateNow(detail.latestVersion, detail.policy.channel)
    if (result) {
      ZNotification.success({
        message: t('systemUpdate.feedback.started'),
      })
    }
  }
  catch {}
}

async function handleRestart() {
  if (checkingRestart.value || !canRestart.value)
    return
  checkingRestart.value = true
  try {
    restartPlan.value = await checkSystemRestart()
    if (restartPlan.value.incompatiblePlugins.length) {
      restartConfirmOpen.value = true
      return
    }
    await restartNow(restartPlan.value)
  }
  catch (error: unknown) {
    ZNotification.error({
      message: errorMessage(error, t('systemUpdate.errors.preRestartCheck')),
    })
  }
  finally {
    checkingRestart.value = false
  }
}

async function handleConfirmRestart() {
  if (!restartPlan.value)
    return
  try {
    await restartNow(restartPlan.value)
    restartConfirmOpen.value = false
  }
  catch (error: unknown) {
    restartConfirmOpen.value = false
    ZNotification.error({
      message: errorMessage(error, t('systemUpdate.errors.restart')),
    })
  }
}

watch(open, (visible) => {
  if (visible) {
    void loadSystem(false).catch(() => undefined)
  }
})

watch(
  () => updateLogs.value.at(-1)?.id,
  (logId, previousLogId) => {
    if (!logId || logId === previousLogId)
      return

    pinUpdateLogsToBottom()
  },
  { flush: 'post' },
)
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('systemUpdate.title')"
    :description="t('systemUpdate.description')"
    type="success"
    width="48rem"
    :show-close="!restarting" :close-on-click-modal="!restarting" :close-on-press-escape="!restarting"
  >
    <template #icon>
      <ArrowUpCircle class="size-4.5 text-z-success" />
    </template>

    <div class="grid gap-3.5">
      <section class="grid gap-4 rounded-z-card bg-z-fill-quaternary px-4 py-4">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="min-w-0">
            <p class="m-0 text-z-xs leading-none font-heavy text-z-text-quaternary">
              Codex Proxy RS
            </p>
            <p class="mt-2 mb-0 text-lg leading-none font-heavy text-z-text">
              {{ t('systemUpdate.application') }}
            </p>
          </div>
          <span
            class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-z-sm font-heavy"
            :class="presentation.status.badge"
          >
            <component
              :is="presentation.status.icon"
              class="size-3.5"
              :class="presentation.status.iconClass"
            />
            {{ presentation.status.label }}
          </span>
        </div>

        <div class="grid gap-2.5 sm:grid-cols-4">
          <div
            v-for="item in presentation.summaryItems"
            :key="item.key"
            class="min-w-0 rounded-z bg-z-bg-container px-3 py-2.5"
          >
            <div class="flex min-w-0 items-center justify-between gap-2">
              <p class="m-0 truncate text-z-xs leading-none font-heavy text-z-text-quaternary">
                {{ item.label }}
              </p>
              <a
                v-if="item.releaseUrl"
                :href="item.releaseUrl"
                target="_blank"
                rel="noreferrer"
                :class="{ invisible: checking || changingChannel }"
                class="inline-flex shrink-0 items-center gap-1 text-z-xs leading-none font-bold text-z-link transition-colors hover:text-z-link-hover"
              >
                {{ t('systemUpdate.releasePage') }}
                <ExternalLink class="size-3" />
              </a>
            </div>
            <p
              class="mt-2 mb-0 h-[1em] truncate font-mono text-z leading-none font-bold text-z-text"
              :title="item.title || item.value"
            >
              <ZSkeleton
                v-if="loading || ((checking || changingChannel) && item.key === 'latest')"
                class="h-full w-20 max-w-full"
                :aria-label="t('systemUpdate.status.checkingVersion')"
              />
              <template v-else>
                {{ item.value }}
              </template>
            </p>
          </div>
        </div>

        <p v-if="updateInfo?.unsupportedReason" class="m-0 text-z-sm text-z-text-secondary">
          {{ updateInfo.unsupportedReason }}
        </p>
        <p
          v-if="updateError"
          role="alert"
          class="m-0 rounded-z bg-z-error-container px-3 py-2 text-z-sm leading-normal font-bold text-z-error-on-container"
        >
          {{ updateError }}
        </p>
        <div
          v-else-if="updateInfo?.warning"
          role="status"
          class="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3.5 gap-y-3 rounded-z bg-z-warning-container px-4 py-5 sm:grid-cols-[auto_minmax(0,1fr)_auto]"
        >
          <span class="grid size-9 place-items-center rounded-z bg-z-warning-container-active text-z-warning-on-container" aria-hidden="true">
            <component :is="updateInfo.retryAt ? Hourglass : TriangleAlert" class="size-4.5" />
          </span>
          <p class="m-0 min-w-0 wrap-anywhere text-z font-bold leading-tight text-z-warning-on-container">
            {{ updateInfo.warning }}
          </p>
          <div v-if="updateInfo.retryAt && updateInfo.retryAtDisplay" class="col-start-2 grid gap-1 sm:col-start-3 sm:text-end">
            <span class="text-z-xs leading-none text-z-text-secondary">{{ t('systemUpdate.retryAt') }}</span>
            <time :datetime="updateInfo.retryAt" class="font-mono text-z-sm font-emphasis tabular-nums text-z-text">{{ updateInfo.retryAtDisplay }}</time>
          </div>
        </div>
        <ZPopover v-if="lastFailedOperation" placement="bottom-start" class="justify-self-start">
          <template #reference>
            <ZButton variant="text" size="small">
              <template #icon>
                <History class="size-3.5" />
              </template>
              {{ t('systemUpdate.status.lastFailed') }}
            </ZButton>
          </template>
          <div class="grid w-80 max-w-[calc(100vw-2rem)] gap-2 p-3 text-z-sm">
            <div v-if="lastFailedOperation.targetVersion || lastFailedOperation.finishedAt" class="flex flex-wrap gap-x-3 gap-y-1 text-z-xs text-z-text-quaternary">
              <span v-if="lastFailedOperation.targetVersion">{{ t('systemUpdate.targetVersion', { version: lastFailedOperation.targetVersion }) }}</span>
              <span v-if="lastFailedOperation.finishedAt">{{ lastFailedOperation.finishedAtDisplay }}</span>
            </div>
            <p class="m-0 wrap-anywhere text-z-text-secondary">
              {{ lastFailedOperation.error || lastFailedOperation.message || t('common.errors.operation') }}
            </p>
          </div>
        </ZPopover>
      </section>

      <section
        v-if="hasReleaseNotes || releaseNotesLoading"
        class="grid gap-2 rounded-z-card bg-z-fill-quaternary px-4 py-3.5"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="m-0 text-z font-heavy text-z-text">
            {{ t('systemUpdate.releaseNotes') }}
          </p>
          <span class="font-mono text-z-xs font-emphasis text-z-text-quaternary">
            <ZSkeleton v-if="releaseNotesLoading" shape="text" class="w-14" aria-hidden="true" />
            <template v-else>{{ presentation.releaseVersion }}</template>
          </span>
        </div>
        <ZScrollbar class="-mx-4" max-height="160px">
          <div
            class="relative px-4"
            :class="{ 'min-h-40': releaseNotesLoading && !hasReleaseNotes }"
            :aria-busy="releaseNotesLoading"
          >
            <div :class="{ invisible: releaseNotesLoading }" :aria-hidden="releaseNotesLoading || undefined">
              <ZMarkdown :source="updateInfo?.notes" />
            </div>
            <div v-if="releaseNotesLoading" class="absolute inset-x-4 top-0 grid h-full content-start gap-3 overflow-hidden py-1" aria-hidden="true">
              <ZSkeleton shape="text" class="w-20" />
              <ZSkeleton shape="text" class="w-4/5" />
              <ZSkeleton shape="text" class="w-3/5" />
            </div>
          </div>
        </ZScrollbar>
      </section>

      <section
        v-if="showUpdateProgress"
        class="overflow-hidden rounded-z-card bg-z-fill-quaternary"
      >
        <header class="flex items-center justify-between gap-3 px-4 pt-3.5 pb-2.5">
          <div class="flex min-w-0 items-center gap-2">
            <Terminal class="size-4 shrink-0 text-z-success" />
            <p class="m-0 text-z leading-none font-heavy text-z-text">
              {{ t('systemUpdate.progress') }}
            </p>
          </div>
          <span
            class="inline-flex h-6 items-center gap-1.5 rounded-full bg-z-fill-quaternary px-2 text-z-xs leading-none font-bold text-z-text-secondary"
            :title="updateStreamError || undefined"
          >
            <i
              class="size-1.5 rounded-full"
              :class="updateStreaming ? 'bg-z-success' : 'bg-z-text-quaternary'"
            />
            {{ presentation.streamStatusLabel }}
          </span>
        </header>

        <ZScrollbar
          ref="updateLogScrollbar"
          height="260px"
        >
          <div class="grid min-h-full gap-2 px-4 pb-4">
            <div
              v-for="log in updateLogRows"
              :key="log.id"
              class="grid grid-cols-[68px_14px_minmax(0,1fr)] items-start gap-2 rounded-z bg-z-bg-container px-3 py-2 font-mono text-z-xs leading-[1.55]"
            >
              <span class="tabular-nums text-z-text-quaternary">{{ log.time }}</span>
              <Circle
                class="mt-1 size-2.5"
                :class="log.classes.marker"
                fill="currentColor"
              />
              <p class="m-0 min-w-0 wrap-break-word" :class="log.classes.text">
                <span v-if="log.step" class="mr-1 text-z-text-quaternary">[{{ log.step }}]</span>
                {{ log.message }}
              </p>
            </div>
          </div>
        </ZScrollbar>
      </section>
    </div>

    <template #footer>
      <div class="mr-auto flex min-w-0 basis-full items-center gap-2 sm:basis-auto">
        <ZSkeleton v-if="loading" class="h-8 w-52" :aria-label="t('systemUpdate.channels.loading')" />
        <ZSegmented
          v-else
          :model-value="selectedChannel"
          :options="channelOptions"
          :aria-label="t('systemUpdate.channels.label')"
          size="small"
          :disabled="!canChangeChannel"
          @update:model-value="handleChannelChange"
        />
        <ZPopover placement="top-start">
          <template #reference>
            <ZButton variant="link" size="small" square :aria-label="t('systemUpdate.channels.help')">
              <CircleHelp class="size-4" />
            </ZButton>
          </template>
          <div class="grid w-72 gap-2 p-3 text-z-sm text-z-text-secondary">
            <p v-for="channel in availableChannels" :key="channel" class="m-0">
              <strong class="font-heavy text-z-text">{{ channelLabels[channel].label }}</strong>
              · {{ channelLabels[channel].description }}
            </p>
            <p class="m-0 text-z-text-quaternary">
              {{ t('systemUpdate.channels.description') }}
            </p>
          </div>
        </ZPopover>
      </div>
      <ZButton
        :loading="checking"
        :disabled="loading || updating || restarting || changingChannel"
        @click="handleCheckUpdates(true)"
      >
        <template #loading>
          <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
        </template>
        <template #icon>
          <RefreshCw class="size-3.5" />
        </template>
        {{ t('systemUpdate.actions.check') }}
      </ZButton>
      <ZButton
        v-if="needRestart"
        type="primary"
        :loading="restarting || checkingRestart"
        :disabled="!canRestart || restartConfirmOpen"
        @click="handleRestart"
      >
        <template #icon>
          <Power class="size-4" />
        </template>
        {{ presentation.restartButtonLabel }}
      </ZButton>
      <ZButton
        v-else-if="hasUpdate || updating"
        type="primary"
        :loading="updating"
        :disabled="!canUpdate"
        @click="handleUpdateRequest"
      >
        <template #icon>
          <Download class="size-4" />
        </template>
        {{ updating ? t('systemUpdate.status.updating') : t('systemUpdate.actions.install') }}
      </ZButton>
    </template>
  </ZDialog>

  <ZConfirmDialog
    v-model="restartConfirmOpen"
    :title="t('systemUpdate.plugins.title')"
    :description="t('systemUpdate.plugins.description')"
    :confirm-text="t('systemUpdate.actions.continueRestart')"
    :loading="restarting"
    @confirm="handleConfirmRestart"
  >
    <div class="grid gap-4">
      <div v-if="restartPlan?.targetVersion" class="flex items-center justify-between gap-3 text-z-sm">
        <span class="text-z-text-secondary">{{ t('systemUpdate.versionAfterRestart') }}</span>
        <span class="font-mono font-emphasis text-z-text">v{{ restartPlan.targetVersion }}</span>
      </div>
      <ul class="m-0 max-h-64 list-none overflow-y-auto p-0" :aria-label="t('systemUpdate.plugins.incompatible')">
        <li v-for="plugin in restartPlan?.incompatiblePlugins" :key="plugin.instanceId" class="flex items-start gap-3 py-3 first:pt-0 last:pb-0">
          <TriangleAlert class="mt-0.5 size-4 shrink-0 text-z-warning" aria-hidden="true" />
          <div class="grid min-w-0 gap-1">
            <strong class="wrap-anywhere text-z-sm font-heavy text-z-text">{{ plugin.name }}</strong>
            <span class="wrap-anywhere text-z-xs leading-relaxed font-normal text-z-text-secondary">{{ plugin.reason }}</span>
          </div>
        </li>
      </ul>
      <p class="m-0 rounded-z bg-z-fill-quaternary px-3 py-2.5 text-z-sm leading-relaxed font-normal text-z-text-secondary">
        {{ t('systemUpdate.plugins.requestErrorHelp') }}
      </p>
    </div>
  </ZConfirmDialog>
</template>
