<script setup lang="ts">
import { computed, nextTick, onUnmounted, useTemplateRef, watch } from 'vue'
import {
  ArrowUpCircle,
  CheckCircle2,
  Circle,
  ExternalLink,
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
import { renderMarkdown } from '@/utils/markdown'

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
  canUpdate,
  loadSystem,
  checkUpdates,
  updateNow,
  restartNow,
  clearRestartTimer,
  connectUpdateEvents,
  disconnectUpdateEvents,
} = useSystemUpdate()

const updateLogScrollbar = useTemplateRef<InstanceType<typeof BaseScrollbar>>('updateLogScrollbar')

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
    key: 'current',
    label: '当前版本',
    value: loading.value ? '...' : version.value?.version ? `v${version.value.version}` : '-',
    title: version.value?.version,
  },
  {
    key: 'latest',
    label: '最新版本',
    value: updateInfo.value?.latestVersion ? `v${updateInfo.value.latestVersion}` : '-',
    title: updateInfo.value?.latestVersion,
    releaseUrl: updateInfo.value?.releaseUrl,
  },
  {
    key: 'build',
    label: '构建',
    value: displayValue(updateInfo.value?.buildTypeLabel),
    title: updateInfo.value?.buildType,
  },
  {
    key: 'deployment',
    label: '部署',
    value: displayValue(version.value?.deploymentModeLabel),
    title: version.value?.deploymentMode,
  },
])

const updateLogRows = computed(() =>
  updateLogs.value.map((item) => ({
    ...item,
    time: formatLogTime(item.at),
  })),
)

const showUpdateProgress = computed(
  () => hasUpdate.value || updating.value || updateSuccess.value || updateLogRows.value.length > 0,
)
const renderedReleaseNotes = computed(() => renderMarkdown(updateInfo.value?.notes))

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

watch(
  () => updateLogs.value.at(-1)?.id,
  async () => {
    await nextTick()
    await updateLogScrollbar.value?.scrollToBottom()
  },
)

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
            class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full px-2.5 text-[12px] font-[760]"
            :class="statusView.badge"
          >
            <component :is="statusView.icon" class="size-3.5" :class="statusView.iconClass" />
            {{ statusView.label }}
          </span>
        </div>

        <div class="grid gap-2.5 sm:grid-cols-4">
          <div
            v-for="item in summaryItems"
            :key="item.key"
            class="min-w-0 rounded-(--cp-input-radius-base) bg-(--cp-bg-surface) px-3 py-2.5"
          >
            <div class="flex min-w-0 items-center justify-between gap-2">
              <p class="m-0 truncate text-[11px] leading-none font-[760] text-(--cp-text-muted)">
                {{ item.label }}
              </p>
              <a
                v-if="item.releaseUrl"
                :href="item.releaseUrl"
                target="_blank"
                rel="noreferrer"
                class="inline-flex shrink-0 items-center gap-1 text-[11px] leading-none font-[720] text-(--cp-info-text) transition-colors hover:text-(--cp-info)"
              >
                发布页
                <ExternalLink class="size-3" />
              </a>
            </div>
            <p
              class="mt-2 mb-0 truncate font-mono text-[13px] leading-none font-[720] text-(--cp-text-primary)"
              :title="item.title || item.value"
            >
              {{ item.value }}
            </p>
          </div>
        </div>

        <p
          v-if="updateError || updateInfo?.warning"
          class="m-0 rounded-(--cp-input-radius-base) bg-(--cp-danger-bg) px-3 py-2 text-[12px] leading-normal font-[720] text-(--cp-danger-text)"
        >
          {{ updateError || updateInfo?.warning }}
        </p>
      </section>

      <section
        v-if="showUpdateProgress"
        class="overflow-hidden rounded-(--cp-card-radius) bg-(--cp-bg-subtle)"
      >
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

        <BaseScrollbar ref="updateLogScrollbar" height="260px" view-class="min-h-full px-4 pb-4">
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
        v-if="renderedReleaseNotes"
        class="grid gap-2 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-3.5"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="m-0 text-[13px] font-[760] text-(--cp-text-primary)">发布说明</p>
          <span class="font-mono text-[11px] font-[650] text-(--cp-text-muted)">
            {{ displayValue(updateInfo?.latestVersion) }}
          </span>
        </div>
        <BaseScrollbar class="-mx-4" max-height="160px" view-class="px-4 pr-5" track-inset="none">
          <div class="release-notes" v-html="renderedReleaseNotes" />
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

<style scoped>
.release-notes {
  color: var(--cp-text-primary);
  font-size: 12px;
  font-weight: 620;
  line-height: 1.65;
  overflow-wrap: anywhere;
}

.release-notes :deep(*) {
  max-width: 100%;
}

.release-notes :deep(:first-child) {
  margin-top: 0;
}

.release-notes :deep(:last-child) {
  margin-bottom: 0;
}

.release-notes :deep(h1),
.release-notes :deep(h2),
.release-notes :deep(h3),
.release-notes :deep(h4) {
  margin: 12px 0 6px;
  color: var(--cp-text-primary);
  font-size: 12px;
  font-weight: 780;
  line-height: 1.4;
}

.release-notes :deep(p) {
  margin: 0 0 8px;
}

.release-notes :deep(ul),
.release-notes :deep(ol) {
  margin: 0 0 8px;
  padding-left: 18px;
}

.release-notes :deep(li) {
  margin: 3px 0;
}

.release-notes :deep(a) {
  color: var(--cp-info-text);
  font-weight: 720;
  text-decoration: none;
}

.release-notes :deep(a:hover) {
  color: var(--cp-info);
}

.release-notes :deep(code) {
  border-radius: 5px;
  background: var(--cp-bg-muted);
  color: var(--cp-text-primary);
  font-family: var(--font-mono);
  font-size: 11px;
  padding: 1px 5px;
}

.release-notes :deep(pre) {
  margin: 8px 0;
  overflow-x: auto;
  border-radius: var(--cp-input-radius-base);
  background: var(--cp-bg-surface);
  padding: 8px 10px;
}

.release-notes :deep(pre code) {
  background: transparent;
  padding: 0;
}

.release-notes :deep(blockquote) {
  margin: 8px 0;
  border-left: 3px solid var(--cp-divider-subtle);
  color: var(--cp-text-secondary);
  padding-left: 10px;
}
</style>
