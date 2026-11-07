<script setup lang="ts">
import type { PluginManagementView } from '@/api'

import { ZButton, ZCard, ZEmpty, ZTag } from '@codex-proxy/ui'

import { Blocks, ExternalLink } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { pluginPageLocation, shortPluginInstanceId } from '@/utils/plugin'

const props = defineProps<{
  views: PluginManagementView[]
  loading: boolean
}>()

const { t } = useI18n({ useScope: 'global' })

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
    <ZEmpty v-if="loading && pageViews.length === 0" :icon="Blocks" :title="t('plugins.extensions.loading')" surface="none" class="flex-1 content-center" role="status" aria-busy="true" />

    <ZEmpty
      v-else-if="pageViews.length === 0"
      class="flex-1 content-center"
      surface="none"
      :icon="Blocks"
      :title="t('plugins.extensions.emptyTitle')"
      :description="t('plugins.extensions.emptyDescription')"
    />

    <div v-else class="grid content-start gap-3 overflow-y-auto sm:grid-cols-2 xl:grid-cols-3">
      <ZCard
        v-for="view in pageViews"
        :key="view.target.instanceId"
        as="article"
        padding="compact"
        class="flex min-w-0 flex-col gap-3"
      >
        <div class="flex min-w-0 items-start gap-3">
          <span class="inline-flex size-9 shrink-0 items-center justify-center rounded-z bg-z-primary-container text-z-primary-on-container">
            <Blocks class="size-4" />
          </span>
          <div class="min-w-0 flex-1">
            <h2 class="m-0 truncate text-z-lg font-heavy text-z-text" :title="view.name">
              {{ view.name }}
            </h2>
            <p class="mt-1 mb-0 truncate font-mono text-z-xs text-z-text-quaternary" :title="view.target.instanceId">
              {{ t('plugins.extensions.configuration', { value: shortPluginInstanceId(view.target.instanceId) }) }}
            </p>
          </div>
          <ZTag size="small">
            {{ t('plugins.extensions.pages', { count: view.pages.length }) }}
          </ZTag>
        </div>

        <p class="m-0 line-clamp-2 text-z-sm leading-relaxed text-z-text-secondary">
          {{ view.pages.map(page => page.title).join('、') }}
        </p>

        <ZButton class="mt-auto self-start" size="small" @click="openView(view)">
          <template #icon>
            <ExternalLink class="size-3.5" />
          </template>
          {{ t('plugins.extensions.openPage') }}
        </ZButton>
      </ZCard>
    </div>
  </div>
</template>
