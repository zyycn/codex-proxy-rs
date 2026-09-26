<script setup lang="ts">
import type { SystemUpdateChannel, SystemUpdateDetail } from '@/api'
import { BaseButton, BaseConfirmModal, BaseMarkdown, BaseModal, BasePopover, BaseScrollbar, BaseSegmented, BaseSkeleton, toast } from '@codex-proxy/ui'

import {
  ArrowUpCircle,
  Circle,
  CircleHelp,
  Download,
  ExternalLink,
  History,
  Power,
  RefreshCw,
  Terminal,
} from '@lucide/vue'
import { storeToRefs } from 'pinia'
import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'
import { normalizeSystemVersion, useSystemUpdateStore } from '@/stores/modules/system-update'
import { formatDateTime, formatTime } from '@/utils/format'
import { errorMessage } from '@/utils/operation'
import {
  resolveSystemUpdateLogClasses,
  resolveSystemUpdatePresentation,
} from './presenter'

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
} = storeToRefs(systemUpdateStore)
const { loadSystem, checkUpdates, changeChannel, updateNow, restartNow } = systemUpdateStore

const updateLogScrollbar = useTemplateRef<InstanceType<typeof BaseScrollbar>>('updateLogScrollbar')
const updateConfirmOpen = shallowRef(false)
const updateConfirmInfo = shallowRef<SystemUpdateDetail | null>(null)
const updateConfirmPreviousTarget = shallowRef('')
const preparingUpdate = shallowRef(false)

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
  previousTargetVersion: updateConfirmPreviousTarget.value,
  confirmedTargetVersion: updateConfirmInfo.value?.latestVersion ?? null,
}))

const updateLogRows = computed(() =>
  updateLogs.value.map(item => ({
    ...item,
    time: formatTime(item.at, '--:--:--'),
    classes: resolveSystemUpdateLogClasses(item.level),
  })),
)

const channelLabels = {
  stable: { label: 'Stable · 正式', description: '仅正式版本' },
  rc: { label: 'RC · 候选', description: '包含 RC 和正式版本' },
  beta: { label: 'Beta · 测试', description: '包含 Beta、RC 和正式版本' },
  alpha: { label: 'Alpha · 早期', description: '包含所有常规预发布和正式版本' },
  exp: { label: 'Exp · 实验', description: '仅当前实验线' },
}
const channelOptions = computed(() => availableChannels.value.map(value => ({
  value,
  label: { stable: 'Stable', rc: 'RC', beta: 'Beta', alpha: 'Alpha', exp: 'Exp' }[value],
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
    if (data?.warning) {
      toast.error(data.warning)
      return
    }
    toast.success(data?.hasUpdate ? '发现可用更新' : '当前没有可用更新')
  }
  catch {}
}

async function handleUpdateRequest() {
  if (preparingUpdate.value || updating.value)
    return

  const previousTargetVersion = normalizeSystemVersion(updateInfo.value?.latestVersion)
  preparingUpdate.value = true
  try {
    const data = await checkUpdates(true)
    if (data?.warning) {
      toast.error(data.warning)
      return
    }
    if (!data?.hasUpdate || !canUpdate.value) {
      toast.success('当前没有可用更新')
      return
    }
    const remoteTargetVersion = normalizeSystemVersion(data.latestVersion)
    if (!remoteTargetVersion) {
      toast.error('远端目标版本为空')
      return
    }
    if (previousTargetVersion && previousTargetVersion !== remoteTargetVersion) {
      updateConfirmPreviousTarget.value = previousTargetVersion
      updateConfirmInfo.value = data
      updateConfirmOpen.value = true
      return
    }
    await runConfirmedUpdate(remoteTargetVersion, data.policy.channel)
  }
  catch {}
  finally {
    preparingUpdate.value = false
  }
}

async function runConfirmedUpdate(targetVersion: string, channel: SystemUpdateChannel) {
  try {
    const result = await updateNow(targetVersion, channel)
    if (result) {
      toast.success('更新已开始')
    }
  }
  catch {}
}

async function handleConfirmUpdate() {
  const targetVersion = normalizeSystemVersion(updateConfirmInfo.value?.latestVersion)
  if (!targetVersion || !updateConfirmInfo.value)
    return

  const channel = updateConfirmInfo.value.policy.channel
  updateConfirmOpen.value = false
  await nextTick()
  await runConfirmedUpdate(targetVersion, channel)
}

async function handleRestart() {
  try {
    await restartNow()
  }
  catch (error: unknown) {
    toast.error(errorMessage(error, '重启失败'))
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
  <BaseModal
    v-model="open"
    title="系统更新"
    description="检查版本、查看发布说明并执行在线更新"
    tone="success"
    size="lg"
    :dismissible="!restarting"
  >
    <template #icon>
      <ArrowUpCircle class="size-4.5 text-cp-success" />
    </template>

    <div class="grid gap-3.5">
      <section class="grid gap-4 rounded-cp-card bg-cp-fill-quaternary px-4 py-4">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="min-w-0">
            <p class="m-0 text-cp-xs leading-none font-heavy text-cp-text-quaternary">
              Codex Proxy RS
            </p>
            <p class="mt-2 mb-0 text-lg leading-none font-heavy text-cp-text">
              应用包更新
            </p>
          </div>
          <span
            class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-cp-sm font-heavy"
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
            class="min-w-0 rounded-cp bg-cp-bg-container px-3 py-2.5"
          >
            <div class="flex min-w-0 items-center justify-between gap-2">
              <p class="m-0 truncate text-cp-xs leading-none font-heavy text-cp-text-quaternary">
                {{ item.label }}
              </p>
              <a
                v-if="item.releaseUrl"
                :href="item.releaseUrl"
                target="_blank"
                rel="noreferrer"
                :class="{ invisible: checking || changingChannel }"
                class="inline-flex shrink-0 items-center gap-1 text-cp-xs leading-none font-bold text-cp-link transition-colors hover:text-cp-link-hover"
              >
                发布页
                <ExternalLink class="size-3" />
              </a>
            </div>
            <p
              class="mt-2 mb-0 h-[1em] truncate font-mono text-cp leading-none font-bold text-cp-text"
              :title="item.title || item.value"
            >
              <BaseSkeleton
                v-if="loading || ((checking || changingChannel) && item.key === 'latest')"
                class="h-full w-20 max-w-full"
                aria-label="正在检查版本"
              />
              <template v-else>
                {{ item.value }}
              </template>
            </p>
          </div>
        </div>

        <p v-if="updateInfo?.unsupportedReason" class="m-0 text-cp-sm text-cp-text-secondary">
          {{ updateInfo.unsupportedReason }}
        </p>
        <p
          v-if="updateError || updateInfo?.warning"
          class="m-0 rounded-cp bg-cp-error-container px-3 py-2 text-cp-sm leading-normal font-bold text-cp-error-on-container"
        >
          {{ updateError || updateInfo?.warning }}
        </p>
        <BasePopover v-if="lastFailedOperation" placement="bottom-start" class="justify-self-start">
          <template #trigger>
            <BaseButton variant="ghost" size="sm">
              <template #icon>
                <History class="size-3.5" />
              </template>
              上次操作失败
            </BaseButton>
          </template>
          <div class="grid w-80 max-w-[calc(100vw-2rem)] gap-2 p-3 text-cp-sm">
            <div v-if="lastFailedOperation.targetVersion || lastFailedOperation.finishedAt" class="flex flex-wrap gap-x-3 gap-y-1 text-cp-xs text-cp-text-quaternary">
              <span v-if="lastFailedOperation.targetVersion">目标版本 v{{ lastFailedOperation.targetVersion }}</span>
              <span v-if="lastFailedOperation.finishedAt">{{ formatDateTime(lastFailedOperation.finishedAt) }}</span>
            </div>
            <p class="m-0 wrap-anywhere text-cp-text-secondary">
              {{ lastFailedOperation.error || lastFailedOperation.message || '操作失败' }}
            </p>
          </div>
        </BasePopover>
      </section>

      <section
        v-if="hasReleaseNotes || releaseNotesLoading"
        class="grid gap-2 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="m-0 text-cp font-heavy text-cp-text">
            发布说明
          </p>
          <span class="font-mono text-cp-xs font-emphasis text-cp-text-quaternary">
            <BaseSkeleton v-if="releaseNotesLoading" shape="text" class="w-14" aria-hidden="true" />
            <template v-else>{{ presentation.releaseVersion }}</template>
          </span>
        </div>
        <BaseScrollbar class="-mx-4" max-height="160px">
          <div
            class="relative px-4"
            :class="{ 'min-h-40': releaseNotesLoading && !hasReleaseNotes }"
            :aria-busy="releaseNotesLoading"
          >
            <div :class="{ invisible: releaseNotesLoading }" :aria-hidden="releaseNotesLoading || undefined">
              <BaseMarkdown :source="updateInfo?.notes" />
            </div>
            <div v-if="releaseNotesLoading" class="absolute inset-x-4 top-0 grid h-full content-start gap-3 overflow-hidden py-1" aria-hidden="true">
              <BaseSkeleton shape="text" class="w-20" />
              <BaseSkeleton shape="text" class="w-4/5" />
              <BaseSkeleton shape="text" class="w-3/5" />
            </div>
          </div>
        </BaseScrollbar>
      </section>

      <section
        v-if="showUpdateProgress"
        class="overflow-hidden rounded-cp-card bg-cp-fill-quaternary"
      >
        <header class="flex items-center justify-between gap-3 px-4 pt-3.5 pb-2.5">
          <div class="flex min-w-0 items-center gap-2">
            <Terminal class="size-4 shrink-0 text-cp-success" />
            <p class="m-0 text-cp leading-none font-heavy text-cp-text">
              更新进度
            </p>
          </div>
          <span
            class="inline-flex h-6 items-center gap-1.5 rounded-full bg-cp-fill-quaternary px-2 text-cp-xs leading-none font-bold text-cp-text-secondary"
            :title="updateStreamError || presentation.streamStatusLabel"
          >
            <i
              class="size-1.5 rounded-full"
              :class="updateStreaming ? 'bg-cp-success' : 'bg-cp-text-quaternary'"
            />
            {{ presentation.streamStatusLabel }}
          </span>
        </header>

        <BaseScrollbar
          ref="updateLogScrollbar"
          height="260px"
        >
          <div class="grid min-h-full gap-2 px-4 pb-4">
            <div
              v-for="log in updateLogRows"
              :key="log.id"
              class="grid grid-cols-[68px_14px_minmax(0,1fr)] items-start gap-2 rounded-cp bg-cp-bg-container px-3 py-2 font-mono text-cp-xs leading-[1.55]"
            >
              <span class="tabular-nums text-cp-text-quaternary">{{ log.time }}</span>
              <Circle
                class="mt-1 size-2.5"
                :class="log.classes.marker"
                fill="currentColor"
              />
              <p class="m-0 min-w-0 wrap-break-word" :class="log.classes.text">
                <span v-if="log.step" class="mr-1 text-cp-text-quaternary">[{{ log.step }}]</span>
                {{ log.message }}
              </p>
            </div>
          </div>
        </BaseScrollbar>
      </section>
    </div>

    <template #footer>
      <div class="mr-auto flex min-w-0 basis-full items-center gap-2 sm:basis-auto">
        <BaseSkeleton v-if="loading" class="h-8 w-52" aria-label="正在读取运行通道" />
        <BaseSegmented
          v-else
          :model-value="selectedChannel"
          :options="channelOptions"
          label="更新通道"
          size="sm"
          :disabled="!canChangeChannel || preparingUpdate || updateConfirmOpen"
          @update:model-value="handleChannelChange"
        />
        <BasePopover placement="top-start">
          <template #trigger>
            <BaseButton variant="ghost" size="sm" square aria-label="更新通道说明">
              <CircleHelp class="size-4" />
            </BaseButton>
          </template>
          <div class="grid w-72 gap-2 p-3 text-cp-sm text-cp-text-secondary">
            <p v-for="channel in availableChannels" :key="channel" class="m-0">
              <strong class="font-heavy text-cp-text">{{ channelLabels[channel].label }}</strong>
              · {{ channelLabels[channel].description }}
            </p>
            <p class="m-0 text-cp-text-quaternary">
              选择仅用于本次检查，重新打开时按运行版本选择通道，不会自动安装或降级
            </p>
          </div>
        </BasePopover>
      </div>
      <BaseButton
        variant="secondary"
        :loading="checking"
        :disabled="loading || updating || restarting || changingChannel || preparingUpdate"
        @click="handleCheckUpdates(true)"
      >
        <template #loading>
          <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
        </template>
        <template #icon>
          <RefreshCw class="size-3.5" />
        </template>
        检查更新
      </BaseButton>
      <BaseButton
        v-if="needRestart"
        variant="primary"
        :loading="restarting"
        :disabled="updating"
        @click="handleRestart"
      >
        <template #icon>
          <Power class="size-4" />
        </template>
        {{ presentation.restartButtonLabel }}
      </BaseButton>
      <BaseButton
        v-else-if="hasUpdate || updating"
        variant="primary"
        :loading="preparingUpdate || updating"
        :disabled="!canUpdate || preparingUpdate"
        @click="handleUpdateRequest"
      >
        <template #icon>
          <Download class="size-4" />
        </template>
        {{ updating ? '更新中' : '下载并更新' }}
      </BaseButton>
    </template>
  </BaseModal>

  <BaseConfirmModal
    v-model="updateConfirmOpen"
    title="发现新的更新版本"
    description="所选通道的最新版本已变化"
    confirm-text="确认更新"
    :loading="updating"
    :confirm-disabled="!updateConfirmInfo?.latestVersion"
    @confirm="handleConfirmUpdate"
  >
    <div class="grid gap-3">
      <div class="grid gap-2 rounded-cp bg-cp-fill-quaternary p-3">
        <div
          v-for="item in presentation.confirmRows"
          :key="item.key"
          class="flex min-w-0 items-center justify-between gap-3"
        >
          <span class="text-cp-sm leading-none font-bold text-cp-text-quaternary">
            {{ item.label }}
          </span>
          <span
            class="truncate font-mono text-cp leading-none font-heavy text-cp-text"
          >
            {{ item.value }}
          </span>
        </div>
      </div>
      <p class="m-0 text-cp-sm leading-relaxed font-emphasis text-cp-text-quaternary">
        点击确认后弹窗会关闭，并按远端最新目标版本开始更新
      </p>
    </div>
  </BaseConfirmModal>
</template>
