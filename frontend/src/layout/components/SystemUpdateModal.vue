<script setup lang="ts">
import { computed, onUnmounted, watch } from 'vue'
import {
  ArrowUpCircle,
  CheckCircle2,
  Circle,
  ExternalLink,
  GitBranch,
  Power,
  RefreshCw,
  Terminal,
  XCircle,
} from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { toast } from '@/components/base/BaseToast'
import { useSystemUpdate, type SystemUpdateLogLevel } from '@/composables/useSystemUpdate'

const open = defineModel<boolean>({ default: false })

const {
  version,
  updateInfo,
  loading,
  checking,
  updating,
  restarting,
  restartCountdown,
  updateError,
  updateSuccess,
  needRestart,
  loadedOnce,
  updateLogs,
  updateStreaming,
  updateStreamError,
  hasUpdate,
  isReleaseBuild,
  canUpdate,
  loadSystem,
  checkUpdates,
  updateNow,
  restartNow,
  clearRestartTimer,
  connectUpdateEvents,
  disconnectUpdateEvents,
} = useSystemUpdate()

const statusView = computed(() => {
  if (updating.value) {
    return {
      label: '更新中',
      icon: RefreshCw,
      badge: 'bg-(--cp-info-bg) text-(--cp-info-text)',
      iconClass: 'text-(--cp-info)',
    }
  }
  if (updateError.value || updateInfo.value?.warning) {
    return {
      label: '异常',
      icon: XCircle,
      badge: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
      iconClass: 'text-(--cp-danger)',
    }
  }
  if (updateSuccess.value) {
    return {
      label: '已更新',
      icon: CheckCircle2,
      badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
      iconClass: 'text-(--cp-success)',
    }
  }
  if (hasUpdate.value) {
    return {
      label: '有新版本',
      icon: ArrowUpCircle,
      badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
      iconClass: 'text-(--cp-success)',
    }
  }
  return {
    label: updateInfo.value ? '已是最新' : '未检查',
    icon: CheckCircle2,
    badge: 'bg-(--cp-bg-muted) text-(--cp-text-secondary)',
    iconClass: 'text-(--cp-text-muted)',
  }
})

const summaryItems = computed(() => [
  {
    label: '当前版本',
    value: loading.value ? '...' : version.value?.version ? `v${version.value.version}` : '-',
    title: version.value?.version,
  },
  {
    label: '最新版本',
    value: updateInfo.value?.latestVersion ? `v${updateInfo.value.latestVersion}` : '-',
    title: updateInfo.value?.latestVersion,
  },
  {
    label: '构建',
    value: displayValue(updateInfo.value?.buildTypeLabel),
    title: updateInfo.value?.buildType,
  },
  {
    label: '部署',
    value: displayValue(version.value?.deploymentModeLabel),
    title: version.value?.deploymentMode,
  },
])

const statusMessage = computed(() => {
  if (updateError.value) return updateError.value
  if (updateSuccess.value && needRestart.value) return '更新已完成，等待重启生效'
  if (updating.value) return '正在替换应用包'
  if (updateInfo.value?.unsupportedReason) return updateInfo.value.unsupportedReason
  if (updateInfo.value?.warning) return updateInfo.value.warning
  if (hasUpdate.value) return `可更新到 v${updateInfo.value?.latestVersion}`
  if (updateInfo.value) return '当前版本已是最新'
  return '尚未检查更新'
})

const statusToneClass = computed(() => {
  if (updateError.value || updateInfo.value?.warning) {
    return 'bg-(--cp-danger-bg) text-(--cp-danger-text)'
  }
  if (updateInfo.value?.unsupportedReason || (hasUpdate.value && !isReleaseBuild.value)) {
    return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  }
  if (updateSuccess.value || hasUpdate.value) {
    return 'bg-(--cp-success-bg) text-(--cp-success-text)'
  }
  return 'bg-(--cp-bg-muted) text-(--cp-text-secondary)'
})

const hasStatusNotice = computed(() =>
  Boolean(
    updateError.value ||
    updateSuccess.value ||
    updateInfo.value?.unsupportedReason ||
    updateInfo.value?.warning ||
    (hasUpdate.value && !isReleaseBuild.value),
  ),
)

const updateLogRows = computed(() =>
  updateLogs.value.map((item) => ({
    ...item,
    time: formatLogTime(item.at),
  })),
)

const streamStatusLabel = computed(() => {
  if (updateStreaming.value) return '实时'
  if (updateStreamError.value) return '断开'
  return '待连接'
})

function displayValue(value: unknown) {
  return value ? String(value) : '-'
}

function formatLogTime(value: string) {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '--:--:--'
  return date.toLocaleTimeString('zh-CN', { hour12: false })
}

function logMarkerClass(level: SystemUpdateLogLevel) {
  if (level === 'success') return 'text-(--cp-success)'
  if (level === 'warning') return 'text-(--cp-warning)'
  if (level === 'error') return 'text-(--cp-danger)'
  return 'text-(--cp-info)'
}

function logTextClass(level: SystemUpdateLogLevel) {
  if (level === 'success') return 'text-(--cp-success)'
  if (level === 'warning') return 'text-(--cp-warning)'
  if (level === 'error') return 'text-(--cp-danger)'
  return 'text-(--cp-text-primary)'
}

async function handleCheckUpdates(force = true) {
  try {
    const data = await checkUpdates(force)
    toast.success(data?.hasUpdate ? '发现可用更新' : '当前已是最新版本')
  } catch (error: any) {
    toast.error(error.message || '检查更新失败')
  }
}

async function handleUpdate() {
  try {
    const result = await updateNow()
    if (result?.needRestart) {
      toast.success('更新完成，请重启服务')
    }
  } catch (error: any) {
    toast.error(error.message || '更新失败')
  }
}

async function handleRestart() {
  toast.success('正在重启服务')
  await restartNow()
}

watch(open, (visible) => {
  if (visible) {
    connectUpdateEvents()
  } else if (!updating.value && !restarting.value) {
    disconnectUpdateEvents()
  }

  if (visible && !loadedOnce.value) {
    void loadSystem(false).catch((error: any) => {
      toast.error(error.message || '加载系统更新信息失败')
    })
  }
})

onUnmounted(() => {
  clearRestartTimer()
  disconnectUpdateEvents()
})
</script>

<template>
  <BaseModal
    v-model="open"
    title="系统更新"
    description="版本、更新状态与实时进度。"
    variant="success"
    width="820px"
    scrollable
    body-max-height="72vh"
    body-view-class="pr-3"
    :close-disabled="updating || restarting"
  >
    <div class="grid gap-3.5">
      <section class="grid gap-4 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-4">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="min-w-0">
            <p class="m-0 text-[11px] leading-none font-[760] text-(--cp-text-muted)">
              Codex Proxy RS
            </p>
            <p class="mt-2 mb-0 text-lg leading-none font-[780] text-(--cp-text-primary)">
              应用包更新
            </p>
          </div>
          <span
            class="inline-flex h-7 items-center gap-1.5 rounded-full px-2.5 text-[12px] font-[760]"
            :class="statusView.badge"
          >
            <component :is="statusView.icon" class="size-3.5" :class="statusView.iconClass" />
            {{ statusView.label }}
          </span>
        </div>

        <div class="grid gap-2.5 sm:grid-cols-4">
          <div
            v-for="item in summaryItems"
            :key="item.label"
            class="min-w-0 rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) px-3 py-2.5"
          >
            <p class="m-0 text-[11px] leading-none font-[760] text-(--cp-text-muted)">
              {{ item.label }}
            </p>
            <p
              class="mt-2 mb-0 truncate font-mono text-[13px] leading-none font-[720] text-(--cp-text-primary)"
              :title="item.title || item.value"
            >
              {{ item.value }}
            </p>
          </div>
        </div>
      </section>

      <section class="grid gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5">
        <div
          v-if="!hasStatusNotice || updateInfo?.releaseUrl"
          class="flex flex-wrap items-center justify-between gap-3"
        >
          <div class="min-w-0">
            <p class="m-0 text-[11px] leading-none font-[760] text-(--cp-text-muted)">状态</p>
            <p
              v-if="!hasStatusNotice"
              class="mt-1.5 mb-0 wrap-break-word text-[13px] leading-normal font-[720] text-(--cp-text-primary)"
            >
              {{ statusMessage }}
            </p>
          </div>
          <a
            v-if="updateInfo?.releaseUrl"
            :href="updateInfo.releaseUrl"
            target="_blank"
            rel="noreferrer"
            class="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-(--cp-input-radius-base) bg-(--cp-info-bg) px-2.5 text-[12px] font-[720] text-(--cp-info-text) no-underline hover:bg-(--cp-info-bg-hover)"
          >
            发布页
            <ExternalLink class="size-3.5" />
          </a>
        </div>

        <div
          v-if="hasStatusNotice"
          class="flex items-center gap-2.5 rounded-(--cp-input-radius-base) px-3 py-2.5"
          :class="statusToneClass"
        >
          <GitBranch
            v-if="hasUpdate && !isReleaseBuild && !updateError && !updateInfo?.warning"
            class="size-3.5 shrink-0"
          />
          <component v-else :is="statusView.icon" class="size-3.5 shrink-0" />
          <p class="m-0 wrap-break-word text-[12px] font-[650] leading-none">
            {{ statusMessage }}
          </p>
        </div>
      </section>

      <section class="overflow-hidden rounded-(--cp-card-radius) bg-(--cp-bg-subtle)">
        <header class="flex items-center justify-between gap-3 px-4 pt-3.5 pb-2.5">
          <div class="flex min-w-0 items-center gap-2">
            <Terminal class="size-4 shrink-0 text-(--cp-success)" />
            <p class="m-0 text-[13px] leading-none font-[760] text-(--cp-text-primary)">更新进度</p>
          </div>
          <span
            class="inline-flex h-6 items-center gap-1.5 rounded-full bg-(--cp-bg-subtle) px-2 text-[11px] leading-none font-[720] text-(--cp-text-secondary)"
            :title="updateStreamError || streamStatusLabel"
          >
            <i
              class="size-1.5 rounded-full"
              :class="updateStreaming ? 'bg-(--cp-success)' : 'bg-(--cp-text-muted)'"
            />
            {{ streamStatusLabel }}
          </span>
        </header>

        <BaseScrollbar max-height="260px" view-class="px-4 pb-4">
          <div v-if="updateLogRows.length" class="grid gap-2">
            <div
              v-for="log in updateLogRows"
              :key="log.id"
              class="grid grid-cols-[68px_14px_minmax(0,1fr)] items-start gap-2 rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) px-3 py-2 font-mono text-[11px] leading-[1.55]"
            >
              <span class="tabular-nums text-(--cp-text-muted)">{{ log.time }}</span>
              <Circle
                class="mt-1 size-2.5"
                :class="logMarkerClass(log.level)"
                fill="currentColor"
              />
              <p class="m-0 min-w-0 wrap-break-word" :class="logTextClass(log.level)">
                <span v-if="log.step" class="mr-1 text-(--cp-text-muted)">[{{ log.step }}]</span>
                {{ log.message }}
              </p>
            </div>
          </div>
          <p v-else class="m-0 py-8 text-center text-[12px] font-[650] text-(--cp-text-muted)">
            暂无进度
          </p>
        </BaseScrollbar>
      </section>

      <section
        v-if="updateInfo?.notes"
        class="grid gap-2 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="m-0 text-[13px] font-[760] text-(--cp-text-primary)">发布说明</p>
          <span class="font-mono text-[11px] font-[650] text-(--cp-text-muted)">
            {{ displayValue(updateInfo?.latestVersion) }}
          </span>
        </div>
        <BaseScrollbar max-height="160px" view-class="pr-2">
          <pre
            class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-[11px] leading-[1.6] font-[620] text-(--cp-text-primary)"
            >{{ updateInfo.notes }}</pre>
        </BaseScrollbar>
      </section>
    </div>

    <template #footer>
      <BaseButton
        variant="default"
        :loading="checking"
        :disabled="loading || updating || restarting"
        @click="handleCheckUpdates(true)"
      >
        <template #icon>
          <RefreshCw class="size-3.5" />
        </template>
        检查更新
      </BaseButton>
      <BaseButton
        v-if="updateSuccess && needRestart"
        variant="success"
        :loading="restarting"
        :disabled="updating"
        @click="handleRestart"
      >
        <template #icon>
          <Power class="size-4" />
        </template>
        {{ restarting ? `重启中 ${restartCountdown}s` : '立即重启' }}
      </BaseButton>
      <BaseButton
        v-else
        variant="success"
        :loading="updating"
        :disabled="!canUpdate"
        @click="handleUpdate"
      >
        <template #icon>
          <ArrowUpCircle class="size-4" />
        </template>
        立即更新
      </BaseButton>
    </template>
  </BaseModal>
</template>
