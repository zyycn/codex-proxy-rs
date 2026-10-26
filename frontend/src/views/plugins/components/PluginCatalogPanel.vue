<script setup lang="ts">
import type { InstalledPlugin } from '../utils/catalog'

import { defineTableColumns, ZButton, ZCard, ZEmpty, ZIconButton, ZInput, ZPagination, ZScrollbar, ZSegmented, ZSelect, ZTable, ZTag } from '@codex-proxy/ui'
import { LayoutGrid, List, Puzzle, Search, Settings2 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { PLUGIN_STATUS_LABELS } from '../constants'
import { currentPluginInstance, pluginStatus, pluginStatusType } from '../utils/catalog'
import PluginCapabilityTags from './PluginCapabilityTags.vue'
import PluginIcon from './PluginIcon.vue'
import PluginStatusNotice from './PluginStatusNotice.vue'

const props = defineProps<{ plugins: InstalledPlugin[], loading: boolean }>()

defineEmits<{ manage: [plugin: InstalledPlugin] }>()

const { t } = useI18n({ useScope: 'global' })

const search = shallowRef('')
const status = shallowRef('all')
const display = shallowRef('cards')
const page = shallowRef(1)
const pageSize = shallowRef(20)
const statusOptions = computed(() => ([
  {
    label: t('common.status.all'),
    value: 'all',
  },
  ...Object.entries(PLUGIN_STATUS_LABELS.value).map(([value, label]) => ({ value, label })),
]))
const displayOptions = computed(() => ([
  {
    label: t('plugins.catalog.cardView'),
    value: 'cards',
    icon: LayoutGrid,
  },
  {
    label: t('plugins.catalog.listView'),
    value: 'list',
    icon: List,
  },
]))
const filtered = computed(() => props.plugins.filter((plugin) => {
  const metadata = plugin.artifact.metadata
  return (status.value === 'all' || pluginStatus(plugin) === status.value)
    && [
      metadata.displayName,
      metadata.pluginId,
      metadata.publisher,
      metadata.description,
    ].some(value => value.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase()))
}))
const rows = computed(() => filtered.value
  .slice((page.value - 1) * pageSize.value, page.value * pageSize.value)
  .map((plugin) => {
    const current = currentPluginInstance(plugin)
    const version = plugin.artifacts.find(artifact => artifact.metadata.sha256 === current?.artifactSha256)?.metadata.version ?? plugin.artifact.metadata.version
    return {
      ...plugin,
      versionLabel: version,
      versionTitle: `${current
        ? t('plugins.catalog.currentVersion')
        : t('plugins.catalog.installedVersion')}：${version}`,
    }
  }))
const pagination = computed(() => ({ currentPage: page.value, pageSize: pageSize.value, total: filtered.value.length }))
watch([search, status, pageSize], () => {
  page.value = 1
})
watch(() => filtered.value.length, (total) => {
  page.value = Math.min(page.value, Math.max(1, Math.ceil(total / pageSize.value)))
})
const columns = computed(() => (defineTableColumns<(typeof rows.value)[number]>([
  {
    key: 'plugin',
    label: t('navigation.plugins'),
    kind: 'identity',
    size: '3xl',
  },
  {
    key: 'status',
    label: t('common.fields.status'),
    kind: 'custom',
    size: 'lg',
  },
  {
    key: 'capabilities',
    label: t('plugins.catalog.provideFunctionality'),
    kind: 'custom',
    size: 'xl',
  },
  {
    key: 'configurations',
    label: t('plugins.catalog.versionCount'),
    kind: 'custom',
    size: 'lg',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'sm',
  },
])))
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-2">
    <div class="flex shrink-0 flex-wrap items-center gap-3 p-1">
      <ZInput v-model="search" :aria-label="t('plugins.catalog.searchForInstalledPlugins')" :placeholder="t('plugins.catalog.searchPluginsPublishers')" class="min-w-0 flex-1 basis-full sm:max-w-72 sm:basis-auto">
        <template #prefix>
          <Search class="size-4" />
        </template>
      </ZInput>
      <ZSelect v-model="status" :options="statusOptions" :aria-label="t('plugins.catalog.pluginStatus')" class="w-36" />
      <ZSegmented v-model="display" :options="displayOptions" :aria-label="t('plugins.catalog.pluginDisplayMode')" display="icon" class="ml-auto w-20 shrink-0" />
    </div>
    <ZEmpty v-if="loading && !plugins.length" :icon="Puzzle" :title="t('plugins.catalog.loading')" surface="none" class="flex-1 content-center" role="status" aria-busy="true" />
    <ZEmpty v-else-if="!filtered.length" :icon="Puzzle" :title="plugins.length ? t('plugins.catalog.noMatchingPlugins') : t('plugins.catalog.installYourFirstPlugin')" :description="plugins.length ? t('plugins.catalog.tryAnotherNameOrStatus') : t('plugins.catalog.emptyDescription')" surface="none" class="flex-1 content-center">
      <template v-if="plugins.length" #action>
        <ZButton @click="search = ''; status = 'all'">
          {{ t('plugins.catalog.clearFilters') }}
        </ZButton>
      </template>
    </ZEmpty>
    <ZScrollbar v-else-if="display === 'cards'" class="min-h-0 flex-1" :aria-busy="loading">
      <ul class="m-0 grid list-none grid-cols-[repeat(auto-fill,minmax(min(100%,30rem),1fr))] items-start gap-4 p-1" :aria-label="t('plugins.catalog.pluginInstalled')">
        <li v-for="plugin in rows" :key="plugin.id">
          <ZCard padding="compact">
            <div class="flex min-w-0 items-start gap-3">
              <PluginIcon :artifact="plugin.artifact" class="size-10 shrink-0 rounded-cp" />
              <div class="grid min-w-0 flex-1 gap-1">
                <div class="flex min-w-0 items-center gap-2">
                  <strong class="truncate text-cp-sm">{{ plugin.artifact.metadata.displayName }}</strong>
                  <ZTag size="small" class="shrink-0" :title="plugin.versionTitle" :aria-label="plugin.versionTitle">
                    <span class="block max-w-28 truncate">{{ plugin.versionLabel }}</span>
                  </ZTag>
                </div>
                <span class="truncate text-cp-xs text-cp-text-secondary">{{ plugin.artifact.metadata.publisher }}</span>
              </div>
              <PluginStatusNotice v-if="currentPluginInstance(plugin)" :instance="currentPluginInstance(plugin)!" />
              <ZTag :type="pluginStatusType(pluginStatus(plugin))" size="small">
                {{ PLUGIN_STATUS_LABELS[pluginStatus(plugin)] }}
              </ZTag>
            </div>
            <p class="my-4 line-clamp-2 min-h-10 text-cp-sm leading-relaxed text-cp-text-secondary">
              {{ plugin.artifact.metadata.description || t('plugins.catalog.noDescription') }}
            </p>
            <PluginCapabilityTags :metadata="plugin.artifact.metadata" />
            <div class="mt-5 flex flex-wrap items-center justify-between gap-3">
              <span class="text-cp-xs text-cp-text-secondary">{{ t('plugins.catalog.versions', { count: plugin.artifacts.length }) }}</span>
              <ZIconButton class="ml-auto" :aria-label="t('plugins.catalog.manage', { displayName: plugin.artifact.metadata.displayName })" variant="solid" size="small" @click="$emit('manage', plugin)">
                <Settings2 class="size-4" />
              </ZIconButton>
            </div>
          </ZCard>
        </li>
      </ul>
    </ZScrollbar>
    <ZTable v-else class="min-h-0 flex-1" :columns="columns" :data="rows" :row-key="row => row.id" :aria-busy="loading">
      <template #plugin="{ row }">
        <div class="flex min-w-0 items-center gap-3">
          <PluginIcon :artifact="row.artifact" class="size-9 rounded-cp" />
          <div class="grid min-w-0 gap-1">
            <div class="flex min-w-0 items-center gap-2">
              <strong class="truncate">{{ row.artifact.metadata.displayName }}</strong>
              <ZTag size="small" class="shrink-0" :title="row.versionTitle" :aria-label="row.versionTitle">
                <span class="block max-w-28 truncate">{{ row.versionLabel }}</span>
              </ZTag>
            </div>
            <span class="truncate text-cp-xs text-cp-text-secondary">{{ row.artifact.metadata.publisher }}</span>
          </div>
        </div>
      </template>
      <template #status="{ row }">
        <div class="flex items-center gap-1.5">
          <PluginStatusNotice v-if="currentPluginInstance(row)" :instance="currentPluginInstance(row)!" />
          <ZTag :type="pluginStatusType(pluginStatus(row))">
            {{ PLUGIN_STATUS_LABELS[pluginStatus(row)] }}
          </ZTag>
        </div>
      </template>
      <template #capabilities="{ row }">
        <PluginCapabilityTags :metadata="row.artifact.metadata" />
      </template>
      <template #configurations="{ row }">
        {{ t('plugins.catalog.versions', { count: row.artifacts.length }) }}
      </template>
      <template #actions="{ row }">
        <ZIconButton size="small" variant="solid" :aria-label="t('plugins.catalog.manage', { displayName: row.artifact.metadata.displayName })" @click="$emit('manage', row)">
          <Settings2 class="size-4" />
        </ZIconButton>
      </template>
    </ZTable>
    <ZPagination v-if="pagination.total > 0" v-bind="pagination" :disabled="loading" @current-change="page = $event" @size-change="pageSize = $event" />
  </div>
</template>
