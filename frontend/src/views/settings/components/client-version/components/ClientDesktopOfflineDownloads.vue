<script setup lang="ts">
import type { ClientDownloadPackage, CodexDesktopWindowsDownloads } from '@/api'

import { ZButton, ZEmpty, ZIconButton, ZSkeleton } from '@codex-proxy/ui'
import { ArrowDownToLine, PackageOpen, RefreshCw } from '@lucide/vue'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'

import ClientInstallCommandList from './ClientInstallCommandList.vue'

defineOptions({ name: 'ClientDesktopOfflineDownloads' })

const props = withDefaults(
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

const { t } = useI18n({ useScope: 'global' })

const downloadDescription = computed(() => {
  const packages = props.downloads?.packages
  if (packages?.length && packages.every(packageItem => packageItem.source === 'official_openai'))
    return t('settings.clientVersion.openAiPackageHelp')
  return t('settings.clientVersion.storePackageHelp')
})

const localInstallCommand = computed(() => ([
  {
    label: t('settings.clientVersion.msixInstallHelp'),
    command: 'Add-AppxPackage -Path .\\downloaded-package.msix',
  },
]))

function packageTitle(packageItem: ClientDownloadPackage): string {
  return packageItem.architecture === 'arm64'
    ? 'Windows ARM64'
    : 'Windows x64'
}

function formatFileSize(value: number | null): string {
  if (!value)
    return t('settings.clientVersion.sizeUnknown')
  const units = ['B', 'KB', 'MB', 'GB']
  let size = value
  let unit = 0
  while (size >= 1000 && unit < units.length - 1) {
    size /= 1000
    unit += 1
  }
  return `${size >= 10 || unit === 0
    ? size.toFixed(0)
    : size.toFixed(1)} ${units[unit]}`
}
</script>

<template>
  <section class="grid gap-3.5 rounded-cp-card bg-cp-fill-quaternary p-4" aria-labelledby="windows-offline-download-title">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div class="flex min-w-0 items-start gap-3">
        <span class="inline-grid size-9 shrink-0 place-items-center rounded-cp bg-cp-info-container text-cp-info">
          <ArrowDownToLine class="size-4.5" />
        </span>
        <div class="min-w-0">
          <div class="flex flex-wrap items-center gap-2">
            <h4 id="windows-offline-download-title" class="m-0 text-cp-lg leading-none font-heavy text-cp-text">
              {{ t('settings.clientVersion.windowsOfflineInstallation') }}
            </h4>
            <span
              v-if="downloads?.cached"
              class="rounded-full bg-cp-bg-container px-2 py-1 text-cp-xs leading-none font-heavy text-cp-text-quaternary"
            >
              {{ t('settings.clientVersion.cached') }}
            </span>
          </div>
          <p class="mt-1.5 mb-0 text-cp-sm leading-[1.45] font-semibold text-cp-text-secondary">
            {{ downloadDescription }}
          </p>
        </div>
      </div>
      <ZIconButton
        :aria-label="loading ? t('settings.clientVersion.refreshingDownloads') : t('settings.clientVersion.refreshOfflineDownloadLinks')" variant="solid"
        :disabled="loading"
        :aria-busy="loading || undefined"
        @click="emit('refresh')"
      >
        <RefreshCw class="size-4" :class="loading && 'animate-spin motion-reduce:animate-none'" />
      </ZIconButton>
    </div>

    <p
      v-if="downloads?.warning"
      class="m-0 rounded-cp bg-cp-warning-container px-3 py-2.5 text-cp-sm leading-[1.45] font-bold text-cp-warning-on-container"
    >
      {{ downloads.warning }}
    </p>

    <div v-if="loading && !downloads" class="grid gap-3 sm:grid-cols-2" :aria-label="t('settings.clientVersion.loadingPackages')">
      <div v-for="index in 2" :key="index" class="grid gap-3 rounded-cp bg-cp-bg-container p-4 shadow-cp-tertiary">
        <ZSkeleton class="h-4 w-28" />
        <ZSkeleton class="h-3 w-44" shape="text" />
        <ZSkeleton class="h-9 w-full" />
      </div>
    </div>

    <ZEmpty
      v-else-if="error && !downloads"
      :title="t('settings.clientVersion.packagesFailed')"
      :description="error"
      :icon="PackageOpen"
      size="small"
    >
      <template #action>
        <ZButton size="small" :loading="loading" @click="emit('retry')">
          {{ t('common.actions.retry') }}
        </ZButton>
      </template>
    </ZEmpty>

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
            {{ packageItem.version ? `v${packageItem.version}` : t('settings.clientVersion.stableChannel') }}
            · {{ formatFileSize(packageItem.sizeBytes) }}
          </p>
          <p v-if="packageItem.expiresAt" class="mt-1.5 mb-0 text-cp-xs font-semibold text-cp-text-quaternary">
            {{ t('settings.clientVersion.linkExpired', { value: packageItem.expiresAtDisplay ?? '—' }) }}
          </p>
        </div>

        <a
          :href="packageItem.downloadUrl"
          target="_blank"
          rel="noopener noreferrer"
          :aria-label="t('settings.clientVersion.downloadMsix', { value: packageTitle(packageItem) })"
          class="inline-flex h-cp-control items-center justify-center gap-2 rounded-cp-sm bg-(--cp-button-primary-bg) px-4 text-cp font-bold text-(--cp-button-primary-color) shadow-cp-tertiary transition-[background-color,transform] hover:bg-(--cp-button-primary-hover-bg) motion-safe:active:translate-y-px"
        >
          {{ t('settings.clientVersion.downloadPackage') }}
          <ArrowDownToLine class="size-4" />
        </a>
      </article>
    </div>

    <ClientInstallCommandList :commands="localInstallCommand" />
  </section>
</template>
