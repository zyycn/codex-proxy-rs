<script setup lang="ts">
import type { ClientDownloadPackage, CodexDesktopWindowsDownloads } from '@/api'
import { ArrowDownToLine, PackageOpen, RefreshCw } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import { formatDateTime } from '@/utils/date'

import ClientInstallCommandList from './ClientInstallCommandList.vue'

defineOptions({ name: 'ClientDesktopOfflineDownloads' })

withDefaults(
  defineProps<{
    downloads?: CodexDesktopWindowsDownloads | null
    loading?: boolean
    error?: string
  }>(),
  {
    downloads: null,
    loading: false,
    error: '',
  },
)

const emit = defineEmits<{
  refresh: []
  retry: []
}>()

const localInstallCommand = [
  {
    label: '下载后使用 PowerShell 安装 / 升级',
    command: 'Add-AppxPackage -Path .\\<下载的安装包>.msix',
  },
]

function packageTitle(packageItem: ClientDownloadPackage): string {
  return packageItem.architecture === 'arm64' ? 'Windows ARM64' : 'Windows x64'
}

function formatFileSize(value: number | null): string {
  if (!value)
    return '大小未知'
  const units = ['B', 'KB', 'MB', 'GB']
  let size = value
  let unit = 0
  while (size >= 1000 && unit < units.length - 1) {
    size /= 1000
    unit += 1
  }
  return `${size >= 10 || unit === 0 ? size.toFixed(0) : size.toFixed(1)} ${units[unit]}`
}
</script>

<template>
  <section class="grid gap-3.5 rounded-cp-card bg-cp-fill-quaternary p-4" aria-labelledby="windows-offline-download-title">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div class="flex min-w-0 items-start gap-3">
        <span class="inline-grid size-9 shrink-0 place-items-center rounded-cp bg-cp-info-bg text-cp-info">
          <ArrowDownToLine class="size-4.5" />
        </span>
        <div class="min-w-0">
          <div class="flex flex-wrap items-center gap-2">
            <h4 id="windows-offline-download-title" class="m-0 text-cp-lg leading-none font-heavy text-cp-text">
              Windows 离线安装
            </h4>
            <span
              v-if="downloads?.cached"
              class="rounded-full bg-cp-bg-container px-2 py-1 text-cp-xs leading-none font-heavy text-cp-text-quaternary"
            >
              已缓存
            </span>
          </div>
          <p class="mt-1.5 mb-0 text-cp-sm leading-[1.45] font-semibold text-cp-text-secondary">
            后端实时校验 Microsoft CDN 直链；请选择与设备一致的架构。
          </p>
        </div>
      </div>
      <BaseIconButton
        :label="loading ? '正在重新提取离线下载链接' : '重新提取离线下载链接'"
        variant="secondary"
        :disabled="loading"
        :aria-busy="loading || undefined"
        @click="emit('refresh')"
      >
        <RefreshCw class="size-4" :class="loading && 'animate-spin motion-reduce:animate-none'" />
      </BaseIconButton>
    </div>

    <p
      v-if="downloads?.warning"
      class="m-0 rounded-cp bg-cp-warning-bg px-3 py-2.5 text-cp-sm leading-[1.45] font-bold text-cp-warning-text"
    >
      {{ downloads.warning }}
    </p>

    <p
      v-if="error && downloads"
      class="m-0 rounded-cp bg-cp-error-bg px-3 py-2.5 text-cp-sm leading-[1.45] font-bold text-cp-error-text"
      role="alert"
    >
      {{ error }}，当前仍显示上次成功结果。
    </p>

    <div v-if="loading && !downloads" class="grid gap-3 sm:grid-cols-2" aria-label="正在提取离线安装包">
      <div v-for="index in 2" :key="index" class="grid gap-3 rounded-cp bg-cp-bg-container p-4 shadow-cp-tertiary">
        <BaseSkeleton class="h-4 w-28" />
        <BaseSkeleton class="h-3 w-44" shape="text" />
        <BaseSkeleton class="h-9 w-full" />
      </div>
    </div>

    <BaseEmpty
      v-else-if="error && !downloads"
      title="离线安装包加载失败"
      :description="error"
      :icon="PackageOpen"
      size="sm"
    >
      <template #action>
        <BaseButton size="sm" :loading="loading" @click="emit('retry')">
          重试
        </BaseButton>
      </template>
    </BaseEmpty>

    <div v-else-if="downloads" class="grid gap-3 sm:grid-cols-2">
      <article
        v-for="packageItem in downloads.packages"
        :key="packageItem.architecture"
        class="grid content-between gap-4 rounded-cp bg-cp-bg-container p-4 shadow-cp-tertiary"
      >
        <div class="min-w-0">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <p class="m-0 text-cp leading-none font-heavy text-cp-text">
              {{ packageTitle(packageItem) }}
            </p>
            <span class="rounded-full bg-cp-fill-tertiary px-2 py-1 font-mono text-cp-xs leading-none font-bold text-cp-text-secondary">
              {{ packageItem.architecture }}
            </span>
          </div>
          <p class="mt-2 mb-0 truncate font-mono text-cp-sm font-semibold text-cp-text" :title="packageItem.fileName">
            {{ packageItem.version ? `v${packageItem.version}` : '稳定通道' }}
            · {{ formatFileSize(packageItem.sizeBytes) }}
          </p>
          <p v-if="packageItem.expiresAt" class="mt-1.5 mb-0 text-cp-xs font-semibold text-cp-text-quaternary">
            链接失效：{{ formatDateTime(packageItem.expiresAt) }}
          </p>
        </div>

        <a
          :href="packageItem.downloadUrl"
          target="_blank"
          rel="noopener noreferrer"
          :aria-label="`下载 ${packageTitle(packageItem)} MSIX`"
          class="inline-flex h-cp-control items-center justify-center gap-2 rounded-cp-sm bg-(--cp-button-primary-bg) px-4 text-cp font-bold text-(--cp-button-primary-color) shadow-cp-tertiary transition-[background-color,transform] hover:bg-(--cp-button-primary-hover-bg) motion-safe:active:translate-y-px"
        >
          下载 MSIX
          <ArrowDownToLine class="size-4" />
        </a>
      </article>
    </div>

    <ClientInstallCommandList :commands="localInstallCommand" />
  </section>
</template>
