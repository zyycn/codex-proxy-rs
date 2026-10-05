<script setup lang="ts">
import type { PluginManagementView } from '@/api'

import { BaseButton, BaseEmpty, BaseTag } from '@codex-proxy/ui'
import { Blocks, ExternalLink } from '@lucide/vue'
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { pluginPageLocation, shortPluginInstanceId } from '@/utils/plugin'

const props = defineProps<{
  views: PluginManagementView[]
  loading: boolean
}>()

const router = useRouter()
const pageViews = computed(() => props.views.filter(view => view.pages.length > 0))

function openView(view: PluginManagementView) {
  const page = view.pages[0]
  if (page)
    void router.push(pluginPageLocation(view, page))
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-4">
    <div v-if="loading && pageViews.length === 0" class="min-h-0 flex-1" aria-busy="true" />

    <BaseEmpty
      v-else-if="pageViews.length === 0"
      class="flex-1 content-center"
      surface="none"
      :icon="Blocks"
      title="暂无插件管理页面"
      description="启用带有管理页面的插件后，可从此处或侧栏进入"
    />

    <div v-else class="grid content-start gap-3 overflow-y-auto sm:grid-cols-2 xl:grid-cols-3">
      <article
        v-for="view in pageViews"
        :key="view.target.instanceId"
        class="flex min-w-0 flex-col gap-3 rounded-cp-card bg-cp-bg-container p-4 shadow-cp-card"
      >
        <div class="flex min-w-0 items-start gap-3">
          <span class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp bg-cp-primary-container text-cp-primary-on-container">
            <Blocks class="size-4" />
          </span>
          <div class="min-w-0 flex-1">
            <h2 class="m-0 truncate text-cp-lg font-heavy text-cp-text" :title="view.name">
              {{ view.name }}
            </h2>
            <p class="mt-1 mb-0 truncate font-mono text-cp-xs text-cp-text-quaternary" :title="view.target.instanceId">
              配置 {{ shortPluginInstanceId(view.target.instanceId) }}
            </p>
          </div>
          <BaseTag size="sm">
            {{ view.pages.length }} 页
          </BaseTag>
        </div>

        <p class="m-0 line-clamp-2 text-cp-sm leading-relaxed text-cp-text-secondary">
          {{ view.pages.map(page => page.title).join('、') }}
        </p>

        <BaseButton class="mt-auto self-start" size="sm" variant="secondary" @click="openView(view)">
          <template #icon>
            <ExternalLink class="size-3.5" />
          </template>
          打开页面
        </BaseButton>
      </article>
    </div>
  </div>
</template>
