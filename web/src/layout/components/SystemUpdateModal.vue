<script setup lang="ts">
import { computed, onUnmounted, shallowRef, watch } from 'vue'
import { ExternalLink, PackageCheck, Power, RefreshCw, RotateCcw } from '@lucide/vue'

import {
  checkSystemUpdates,
  getSystemVersion,
  performSystemUpdate,
  restartSystem,
  rollbackSystemUpdate,
  type SystemUpdateInfo,
  type SystemVersion,
} from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { toast } from '@/components/base/BaseToast'

const open = defineModel<boolean>({ default: false })

const version = shallowRef<SystemVersion | null>(null)
const updateInfo = shallowRef<SystemUpdateInfo | null>(null)
const loading = shallowRef(false)
const checking = shallowRef(false)
const updating = shallowRef(false)
const rollingBack = shallowRef(false)
const restarting = shallowRef(false)
const reconnecting = shallowRef(false)
const reconnectMessage = shallowRef('')
let disposed = false
let loadedOnce = false

const canUpdate = computed(
  () =>
    Boolean(updateInfo.value?.hasUpdate) &&
    Boolean(updateInfo.value?.updateSupported) &&
    !updating.value &&
    !checking.value,
)

const updateStatus = computed(() => {
  if (updateInfo.value?.warning) return '检查失败'
  if (!updateInfo.value) return '未检查'
  if (updateInfo.value.hasUpdate) return '发现新版本'
  return '已是最新'
})

const updateStatusClass = computed(() => {
  if (updateInfo.value?.warning) return 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
  if (updateInfo.value?.hasUpdate) return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  return 'bg-(--cp-success-bg) text-(--cp-success-text)'
})

const deploymentStatusClass = computed(() =>
  version.value?.deploymentMode === 'docker'
    ? 'bg-(--cp-info-bg) text-(--cp-info-text)'
    : 'bg-(--cp-bg-muted) text-(--cp-text-secondary)',
)

function displayValue(value: unknown) {
  return value ? String(value) : '-'
}

function sleep(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms))
}

async function loadSystem() {
  try {
    loading.value = true
    const [versionData, updateData] = await Promise.all([
      getSystemVersion(),
      checkSystemUpdates(false),
    ])
    version.value = versionData
    updateInfo.value = updateData
    loadedOnce = true
  } catch (error: any) {
    toast.error(error.message || '加载系统更新信息失败')
  } finally {
    loading.value = false
  }
}

async function handleCheckUpdates(force = true) {
  if (checking.value) return

  try {
    checking.value = true
    updateInfo.value = await checkSystemUpdates(force)
    toast.success(updateInfo.value.hasUpdate ? '发现可用更新' : '当前已是最新版本')
  } catch (error: any) {
    toast.error(error.message || '检查更新失败')
  } finally {
    checking.value = false
  }
}

async function waitForReconnect(targetVersion: string) {
  reconnecting.value = true
  reconnectMessage.value = '等待服务重启...'
  for (let attempt = 0; attempt < 40 && !disposed; attempt += 1) {
    await sleep(3000)
    try {
      const data = await getSystemVersion()
      version.value = data
      reconnectMessage.value = `服务已响应，当前版本 ${data.version}`
      if (!targetVersion || data.version === targetVersion) {
        toast.success('系统已恢复')
        await handleCheckUpdates(false)
        break
      }
    } catch {
      reconnectMessage.value = '服务暂时不可用，继续等待...'
    }
  }
  reconnecting.value = false
}

async function handleUpdate() {
  if (!canUpdate.value || !updateInfo.value) return

  try {
    updating.value = true
    const result = await performSystemUpdate({
      confirmBackup: updateInfo.value.requiresBackup,
    })
    toast.success('更新任务已启动')
    if (result.needReconnect) {
      await waitForReconnect(result.targetVersion)
    }
  } catch (error: any) {
    toast.error(error.message || '启动更新失败')
  } finally {
    updating.value = false
  }
}

async function handleRollback() {
  if (rollingBack.value) return

  try {
    rollingBack.value = true
    await rollbackSystemUpdate()
    toast.success('回滚任务已启动')
  } catch (error: any) {
    toast.error(error.message || '启动回滚失败')
  } finally {
    rollingBack.value = false
  }
}

async function handleRestart() {
  if (restarting.value) return

  try {
    restarting.value = true
    await restartSystem()
    toast.success('重启已触发')
    await waitForReconnect(version.value?.version || '')
  } catch (error: any) {
    toast.error(error.message || '重启失败')
  } finally {
    restarting.value = false
  }
}

watch(open, (visible) => {
  if (visible && !loadedOnce) {
    void loadSystem()
  }
})

onUnmounted(() => {
  disposed = true
})
</script>

<template>
  <BaseModal
    v-model="open"
    title="系统更新"
    description="检查版本并触发 Docker 在线更新。"
    variant="info"
    width="780px"
    scrollable
    body-max-height="68vh"
    body-view-class="pr-4"
    :close-disabled="updating || rollingBack || restarting"
  >
    <div class="grid gap-4">
      <section class="grid gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-4">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <p class="m-0 text-[13px] font-[760] text-(--cp-text-primary)">运行版本</p>
            <p class="mt-1.5 mb-0 font-mono text-[12px] font-[650] text-(--cp-text-secondary)">
              {{ loading ? '加载中...' : displayValue(version?.version) }}
            </p>
          </div>
          <span
            class="inline-flex h-7 items-center rounded-full px-2.5 text-[12px] font-[760]"
            :class="deploymentStatusClass"
          >
            {{ loading ? '加载中...' : displayValue(version?.deploymentMode) }}
          </span>
        </div>

        <div class="grid gap-3 sm:grid-cols-2">
          <div class="min-w-0">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">镜像</p>
            <p
              class="mt-1.5 mb-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              :title="version?.image || '-'"
            >
              {{ displayValue(version?.image) }}
            </p>
          </div>
          <div class="min-w-0">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">Git SHA</p>
            <p
              class="mt-1.5 mb-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
            >
              {{ displayValue(version?.gitSha) }}
            </p>
          </div>
        </div>
      </section>

      <section class="grid gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-4">
        <div class="flex flex-wrap items-center gap-3">
          <span
            class="inline-flex h-8 items-center rounded-full px-3 text-[13px] font-[760]"
            :class="updateStatusClass"
          >
            {{ updateStatus }}
          </span>
          <span class="font-mono text-[13px] font-[650] text-(--cp-text-secondary)">
            {{ displayValue(updateInfo?.currentVersion) }} ->
            {{ displayValue(updateInfo?.latestVersion) }}
          </span>
          <a
            v-if="updateInfo?.releaseUrl"
            :href="updateInfo.releaseUrl"
            target="_blank"
            rel="noreferrer"
            class="inline-flex items-center gap-1.5 text-[13px] font-[720] text-(--cp-info-text) no-underline"
          >
            Release
            <ExternalLink class="size-3.5" />
          </a>
        </div>

        <div
          v-if="updateInfo?.unsupportedReason"
          class="rounded-(--cp-input-radius-base) bg-(--cp-warning-bg) px-3 py-2.5 text-[12px] leading-normal font-[650] text-(--cp-warning-text)"
        >
          {{ updateInfo.unsupportedReason }}
        </div>

        <div
          v-if="updateInfo?.warning"
          class="rounded-(--cp-input-radius-base) bg-(--cp-danger-bg) px-3 py-2.5 text-[12px] leading-normal font-[650] text-(--cp-danger-text)"
        >
          {{ updateInfo.warning }}
        </div>

        <div
          v-if="reconnecting"
          class="rounded-(--cp-input-radius-base) bg-(--cp-info-bg) px-3 py-2.5 text-[12px] leading-normal font-[650] text-(--cp-info-text)"
        >
          {{ reconnectMessage }}
        </div>

        <div class="grid gap-3 sm:grid-cols-2">
          <div class="min-w-0">
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">目标镜像</p>
            <p
              class="mt-1.5 mb-0 truncate font-mono text-[12px] font-[650] text-(--cp-text-primary)"
              :title="updateInfo?.targetImage || '-'"
            >
              {{ displayValue(updateInfo?.targetImage) }}
            </p>
          </div>
          <div>
            <p class="m-0 text-[11px] font-[760] text-(--cp-text-muted)">备份确认</p>
            <p class="mt-1.5 mb-0 text-[12px] font-[720] text-(--cp-text-primary)">
              {{ updateInfo?.requiresBackup ? '需要' : '不需要' }}
            </p>
          </div>
        </div>
      </section>

      <section
        v-if="updateInfo?.notes"
        class="grid gap-3 rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-4 py-4"
      >
        <p class="m-0 text-[13px] font-[760] text-(--cp-text-primary)">发布说明</p>
        <BaseScrollbar max-height="260px" view-class="pr-2">
          <pre
            class="m-0 whitespace-pre-wrap break-words font-mono text-[11px] leading-[1.65] font-[620] text-(--cp-text-primary)"
            >{{ updateInfo.notes }}</pre
          >
        </BaseScrollbar>
      </section>
    </div>

    <template #footer>
      <BaseButton
        variant="default"
        :loading="checking"
        :disabled="loading"
        @click="handleCheckUpdates(true)"
      >
        <template #icon>
          <RefreshCw class="size-4" />
        </template>
        检查
      </BaseButton>
      <BaseButton variant="default" :loading="rollingBack" @click="handleRollback">
        <template #icon>
          <RotateCcw class="size-4" />
        </template>
        回滚
      </BaseButton>
      <BaseButton variant="default" :loading="restarting" @click="handleRestart">
        <template #icon>
          <Power class="size-4" />
        </template>
        重启
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="updating"
        :disabled="!canUpdate"
        @click="handleUpdate"
      >
        <template #icon>
          <PackageCheck class="size-4" />
        </template>
        一键更新
      </BaseButton>
    </template>
  </BaseModal>
</template>
