<script setup lang="ts">
import type { InstalledPlugin } from '../utils/catalog'
import { BaseButton, BaseCard, BaseEmpty, BaseIconButton, BaseInput, BaseScrollbar, BaseSegmented, BaseSelect, BaseTable, BaseTablePagination, BaseTag, defineTableColumns } from '@codex-proxy/ui'
import { CircleAlert, LayoutGrid, List, Puzzle, Search, Settings2 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { currentPluginInstance, PLUGIN_STATUS_LABELS, pluginStatus, pluginStatusType } from '../utils/catalog'
import PluginCapabilityTags from './PluginCapabilityTags.vue'
import PluginIcon from './PluginIcon.vue'

const props = defineProps<{ plugins: InstalledPlugin[], loading: boolean }>()
defineEmits<{ manage: [plugin: InstalledPlugin] }>()
const search = shallowRef('')
const status = shallowRef('all')
const display = shallowRef('cards')
const page = shallowRef(1)
const pageSize = shallowRef(20)
const statusOptions = [{ label: '全部状态', value: 'all' }, ...Object.entries(PLUGIN_STATUS_LABELS).map(([value, label]) => ({ value, label }))]
const displayOptions = [{ label: '卡片视图', value: 'cards', icon: LayoutGrid }, { label: '列表视图', value: 'list', icon: List }]
const filtered = computed(() => props.plugins.filter((plugin) => {
  const metadata = plugin.artifact.metadata
  return (status.value === 'all' || pluginStatus(plugin) === status.value)
    && [metadata.displayName, metadata.pluginId, metadata.publisher, metadata.description].some(value => value.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase()))
}))
const rows = computed(() => filtered.value
  .slice((page.value - 1) * pageSize.value, page.value * pageSize.value)
  .map((plugin) => {
    const current = currentPluginInstance(plugin)
    const version = plugin.artifacts.find(artifact => artifact.metadata.sha256 === current?.artifactSha256)?.metadata.version ?? plugin.artifact.metadata.version
    return {
      ...plugin,
      versionLabel: version,
      versionTitle: `${current ? '当前版本' : '已安装版本'}：${version}`,
    }
  }))
const pagination = computed(() => ({ currentPage: page.value, pageSize: pageSize.value, total: filtered.value.length }))
watch([search, status, pageSize], () => {
  page.value = 1
})
watch(() => filtered.value.length, (total) => {
  page.value = Math.min(page.value, Math.max(1, Math.ceil(total / pageSize.value)))
})
const columns = defineTableColumns<(typeof rows.value)[number]>([
  { key: 'plugin', label: '插件', kind: 'identity', size: '3xl' },
  { key: 'status', label: '状态', kind: 'custom', size: 'lg' },
  { key: 'capabilities', label: '提供功能', kind: 'custom', size: 'xl' },
  { key: 'configurations', label: '已安装版本', kind: 'custom', size: 'lg' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm' },
])
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-2">
    <div class="flex shrink-0 flex-wrap items-center gap-3 p-1">
      <BaseInput v-model="search" aria-label="搜索已安装插件" placeholder="搜索插件、发布者" class="min-w-0 flex-1 basis-full sm:max-w-72 sm:basis-auto">
        <template #prefix>
          <Search class="size-4" />
        </template>
      </BaseInput>
      <BaseSelect v-model="status" :options="statusOptions" aria-label="插件状态" class="w-36" />
      <BaseSegmented v-model="display" :options="displayOptions" label="插件显示方式" display="icon" class="ml-auto w-20 shrink-0" />
    </div>
    <div v-if="loading && !plugins.length" class="min-h-0 flex-1" aria-busy="true" />
    <BaseEmpty v-else-if="!filtered.length" :icon="Puzzle" :title="plugins.length ? '没有匹配的插件' : '安装第一个插件'" :description="plugins.length ? '试试其他名称或状态' : '从上方安装插件，查看权限后即可开始使用'" surface="none" class="flex-1 content-center">
      <template v-if="plugins.length" #action>
        <BaseButton variant="secondary" @click="search = ''; status = 'all'">
          清除筛选
        </BaseButton>
      </template>
    </BaseEmpty>
    <BaseScrollbar v-else-if="display === 'cards'" class="min-h-0 flex-1" :aria-busy="loading">
      <ul class="m-0 grid list-none grid-cols-[repeat(auto-fill,minmax(min(100%,30rem),1fr))] items-start gap-4 p-1" aria-label="已安装插件">
        <li v-for="plugin in rows" :key="plugin.id">
          <BaseCard padding="compact">
            <div class="flex min-w-0 items-start gap-3">
              <PluginIcon :artifact="plugin.artifact" class="size-10 shrink-0 rounded-cp" />
              <div class="grid min-w-0 flex-1 gap-1">
                <div class="flex min-w-0 items-center gap-2">
                  <strong class="truncate text-cp-sm">{{ plugin.artifact.metadata.displayName }}</strong>
                  <BaseTag size="sm" class="shrink-0" :title="plugin.versionTitle" :aria-label="plugin.versionTitle">
                    <span class="block max-w-28 truncate">{{ plugin.versionLabel }}</span>
                  </BaseTag>
                </div>
                <span class="truncate text-cp-xs text-cp-text-secondary">{{ plugin.artifact.metadata.publisher }}</span>
              </div>
              <BaseTag :type="pluginStatusType(pluginStatus(plugin))" size="sm">
                <CircleAlert v-if="pluginStatus(plugin) === 'failed'" class="mr-1 size-3.5" />
                {{ PLUGIN_STATUS_LABELS[pluginStatus(plugin)] }}
              </BaseTag>
            </div>
            <p class="my-4 line-clamp-2 min-h-10 text-cp-sm leading-relaxed text-cp-text-secondary">
              {{ plugin.artifact.metadata.description || '发布者未提供说明' }}
            </p>
            <PluginCapabilityTags :metadata="plugin.artifact.metadata" />
            <div class="mt-5 flex flex-wrap items-center justify-between gap-3">
              <span class="text-cp-xs text-cp-text-secondary">{{ plugin.artifacts.length }} 个版本</span>
              <BaseIconButton class="ml-auto" :label="`管理 ${plugin.artifact.metadata.displayName}`" variant="secondary" size="sm" @click="$emit('manage', plugin)">
                <Settings2 class="size-4" />
              </BaseIconButton>
            </div>
          </BaseCard>
        </li>
      </ul>
    </BaseScrollbar>
    <BaseTable v-else class="min-h-0 flex-1" :columns="columns" :rows="rows" :row-key="row => row.id" :aria-busy="loading">
      <template #plugin="{ row }">
        <div class="flex min-w-0 items-center gap-3">
          <PluginIcon :artifact="row.artifact" class="size-9 rounded-cp" />
          <div class="grid min-w-0 gap-1">
            <div class="flex min-w-0 items-center gap-2">
              <strong class="truncate">{{ row.artifact.metadata.displayName }}</strong>
              <BaseTag size="sm" class="shrink-0" :title="row.versionTitle" :aria-label="row.versionTitle">
                <span class="block max-w-28 truncate">{{ row.versionLabel }}</span>
              </BaseTag>
            </div>
            <span class="truncate text-cp-xs text-cp-text-secondary">{{ row.artifact.metadata.publisher }}</span>
          </div>
        </div>
      </template>
      <template #status="{ row }">
        <BaseTag :type="pluginStatusType(pluginStatus(row))">
          <CircleAlert v-if="pluginStatus(row) === 'failed'" class="mr-1 size-3.5" />
          {{ PLUGIN_STATUS_LABELS[pluginStatus(row)] }}
        </BaseTag>
      </template>
      <template #capabilities="{ row }">
        <PluginCapabilityTags :metadata="row.artifact.metadata" />
      </template>
      <template #configurations="{ row }">
        {{ row.artifacts.length }} 个版本
      </template>
      <template #actions="{ row }">
        <BaseIconButton size="sm" variant="secondary" :label="`管理 ${row.artifact.metadata.displayName}`" @click="$emit('manage', row)">
          <Settings2 class="size-4" />
        </BaseIconButton>
      </template>
    </BaseTable>
    <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="page = $event" @page-size-change="pageSize = $event" />
  </div>
</template>
