<script setup lang="ts">
import { BaseButton, BaseEmpty, BaseIconButton, BasePageHeader, BaseSegmented } from '@codex-proxy/ui'
import { Blocks, CircleAlert, RefreshCw } from '@lucide/vue'
import { storeToRefs } from 'pinia'
import { computed, onMounted, shallowRef } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { usePluginViewsStore } from '@/stores/modules/plugin-views'
import { pluginPageLocation } from '@/utils/plugin'
import PluginManagementFrame from './PluginManagementFrame.vue'
import PluginPageLoading from './PluginPageLoading.vue'

const route = useRoute()
const router = useRouter()
const directory = usePluginViewsStore()
const { pageViews, loading, loaded, loadError } = storeToRefs(directory)
const frameReload = shallowRef(0)

const instanceId = computed(() => routeParam(route.params.instanceId))
const pageId = computed(() => routeParam(route.params.pageId))
const view = computed(() => pageViews.value.find(item => item.target.instanceId === instanceId.value) ?? null)
const page = computed(() => view.value?.pages.find(item => item.id === pageId.value) ?? null)
const pageOptions = computed(() => view.value?.pages.map(item => ({
  label: item.title,
  value: item.id,
})) ?? [])
const selectedPageId = computed({
  get: () => pageId.value,
  set: (nextPageId: string) => {
    const currentView = view.value
    const nextPage = currentView?.pages.find(item => item.id === nextPageId)
    if (currentView && nextPage)
      void router.push(pluginPageLocation(currentView, nextPage))
  },
})
const frameKey = computed(() => {
  const currentView = view.value
  const currentPage = page.value
  if (!currentView || !currentPage)
    return ''
  const target = currentView.target
  return [target.instanceId, target.artifactSha256, target.revision, currentPage.id, frameReload.value].join(':')
})

function routeParam(value: string | string[] | undefined) {
  return Array.isArray(value) ? (value[0] ?? '') : (value ?? '')
}

async function refreshDirectory() {
  try {
    await directory.refresh()
    frameReload.value += 1
  }
  catch {
    // Store 保留可展示的错误；旧目录继续留在页面上，避免把暂时失败误判为撤销。
  }
}

onMounted(() => {
  void directory.ensureLoaded().catch(() => undefined)
})
</script>

<template>
  <div class="flex min-w-0 w-full flex-col gap-5">
    <BasePageHeader
      :title="page?.title ?? '插件页面'"
      :description="page?.description || (view ? `使用插件提供的功能，当前配置为「${view.name}」` : '加载已启用插件的管理页面')"
    >
      <template #actions>
        <BaseIconButton label="刷新页面" variant="secondary" :loading="loading" @click="refreshDirectory">
          <template #loading>
            <RefreshCw class="size-4 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4" />
        </BaseIconButton>
      </template>
    </BasePageHeader>

    <PluginPageLoading v-if="loading && !loaded" />

    <BaseEmpty
      v-else-if="!view || !page"
      class="min-h-0 flex-1 content-center"
      :icon="Blocks"
      title="插件页面不可用"
      :description="loadError || '该配置或页面未发布，可能已停用或切换版本'"
    >
      <template #action>
        <BaseButton variant="secondary" @click="router.push('/plugins')">
          返回插件管理
        </BaseButton>
      </template>
    </BaseEmpty>

    <div
      v-else-if="loadError"
      class="flex shrink-0 flex-wrap items-center gap-2 rounded-cp bg-cp-warning-container px-4 py-3 text-cp-warning-on-container"
      role="alert"
    >
      <CircleAlert class="size-4 shrink-0" />
      <p class="m-0 min-w-0 flex-1 text-cp-sm font-semibold">
        页面目录刷新失败，当前显示上次成功加载的版本：{{ loadError }}
      </p>
      <BaseButton size="sm" variant="secondary" :loading="loading" @click="refreshDirectory">
        重试
      </BaseButton>
    </div>

    <section v-if="view && page" class="flex min-w-0 flex-col gap-4">
      <BaseSegmented
        v-if="view.pages.length > 1"
        v-model="selectedPageId"
        class="w-max max-w-full shrink-0 overflow-x-auto"
        :options="pageOptions"
        label="插件页面"
        size="sm"
      />
      <PluginManagementFrame
        :key="frameKey"
        :view="view"
        :page="page"
        @stale="refreshDirectory"
      />
    </section>
  </div>
</template>
