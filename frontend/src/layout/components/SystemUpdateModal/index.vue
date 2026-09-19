<script setup lang="ts">
import type { SystemUpdateDetail } from '@/api'
import {
  ArrowUpCircle,
  Circle,
  ExternalLink,
  Power,
  RefreshCw,
  Terminal,
} from '@lucide/vue'

import { storeToRefs } from 'pinia'
import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseMarkdown from '@/components/base/BaseMarkdown/index.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { toast } from '@/components/base/BaseToast'
import { normalizeSystemVersion, useSystemUpdateStore } from '@/stores/modules/system-update'
import { errorMessage } from '@/utils/async'
import { formatTime } from '@/utils/date'
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
  updating,
  restarting,
  updateError,
  updateSuccess,
  needRestart,
  loadedOnce,
  updateLogs,
  updateStreaming,
  updateStreamError,
  hasUpdate,
  canUpdate,
} = storeToRefs(systemUpdateStore)
const { loadSystem, checkUpdates, updateNow, restartNow } = systemUpdateStore

const updateLogScrollbar = useTemplateRef<InstanceType<typeof BaseScrollbar>>('updateLogScrollbar')
const updateConfirmOpen = shallowRef(false)
const updateConfirmInfo = shallowRef<SystemUpdateDetail | null>(null)
const updateConfirmPreviousTarget = shallowRef('')
const preparingUpdate = shallowRef(false)

const presentation = computed(() => resolveSystemUpdatePresentation({
  version: version.value,
  updateInfo: updateInfo.value,
  loading: loading.value,
  restarting: restarting.value,
  updating: updating.value,
  updateError: updateError.value,
  updateSuccess: updateSuccess.value,
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

const hasReleaseNotes = computed(() => Boolean(updateInfo.value?.notes?.trim()))

const showUpdateProgress = computed(
  () => hasUpdate.value || updating.value || restarting.value || updateLogRows.value.length > 0,
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
    if (!data?.hasUpdate) {
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
    await runConfirmedUpdate(remoteTargetVersion)
  }
  catch {}
  finally {
    preparingUpdate.value = false
  }
}

async function runConfirmedUpdate(targetVersion: string) {
  try {
    const result = await updateNow(targetVersion)
    if (result?.needRestart) {
      toast.success('更新完成，请重启服务')
    }
  }
  catch {}
}

async function handleConfirmUpdate() {
  const targetVersion = normalizeSystemVersion(updateConfirmInfo.value?.latestVersion)
  if (!targetVersion)
    return

  updateConfirmOpen.value = false
  await nextTick()
  await runConfirmedUpdate(targetVersion)
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
  if (visible && !loadedOnce.value) {
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
    :dismissible="!updating && !restarting"
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
                class="inline-flex shrink-0 items-center gap-1 text-cp-xs leading-none font-bold text-cp-link transition-colors hover:text-cp-link-hover"
              >
                发布页
                <ExternalLink class="size-3" />
              </a>
            </div>
            <p
              class="mt-2 mb-0 truncate font-mono text-cp leading-none font-bold text-cp-text"
              :title="item.title || item.value"
            >
              {{ item.value }}
            </p>
          </div>
        </div>

        <p
          v-if="updateError || updateInfo?.warning"
          class="m-0 rounded-cp bg-cp-error-container px-3 py-2 text-cp-sm leading-normal font-bold text-cp-error-on-container"
        >
          {{ updateError || updateInfo?.warning }}
        </p>
      </section>

      <section
        v-if="hasReleaseNotes"
        class="grid gap-2 rounded-cp-card bg-cp-fill-quaternary px-4 py-3.5"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="m-0 text-cp font-heavy text-cp-text">
            发布说明
          </p>
          <span class="font-mono text-cp-xs font-emphasis text-cp-text-quaternary">
            {{ presentation.releaseVersion }}
          </span>
        </div>
        <BaseScrollbar class="-mx-4" max-height="160px">
          <div class="px-4">
            <BaseMarkdown :source="updateInfo?.notes" />
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
          v-if="updateLogRows.length"
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
        <div v-else class="grid h-30 place-items-center px-4 pb-4">
          <BaseEmpty title="暂无进度" :icon="Terminal" size="sm" surface="none" />
        </div>
      </section>
    </div>

    <template #footer>
      <BaseButton
        variant="secondary"
        :loading="checking"
        :disabled="loading || updating || restarting"
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
        v-if="updateSuccess && needRestart"
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
          <ArrowUpCircle class="size-4" />
        </template>
        立即更新
      </BaseButton>
    </template>
  </BaseModal>

  <BaseConfirmModal
    v-model="updateConfirmOpen"
    title="发现新的更新版本"
    description="检测到远端 latest 与当前显示的目标版本不一致"
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
