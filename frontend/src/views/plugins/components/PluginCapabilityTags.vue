<script setup lang="ts">
import type { PluginArtifactMetadata } from '@/api'
import { BasePopover, BaseTag } from '@codex-proxy/ui'
import { computed, shallowRef, useId } from 'vue'
import { pluginCapabilityLabel } from '../utils/model'

const props = withDefaults(defineProps<{
  metadata: PluginArtifactMetadata
  primary?: boolean
}>(), { primary: false })

const open = shallowRef(false)
const contentId = useId()
const entries = computed(() => Object.entries(props.metadata.contributes))
const visible = computed(() => entries.value.slice(0, 2))
const hidden = computed(() => entries.value.slice(2))
</script>

<template>
  <div class="flex min-w-0 items-center gap-1 whitespace-nowrap">
    <BaseTag v-for="[capability, contribution] in visible" :key="capability" size="sm" :type="primary ? 'primary' : 'neutral'" class="min-w-0">
      <span class="block truncate" :title="`${pluginCapabilityLabel(capability)} · ${contribution.id}`">{{ pluginCapabilityLabel(capability) }}</span>
    </BaseTag>
    <BasePopover v-if="hidden.length" v-model="open" trigger="hover-click" placement="top" class="shrink-0">
      <template #trigger>
        <button
          type="button"
          class="cursor-pointer rounded-cp border-0 bg-transparent p-0 outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          :aria-label="`另有 ${hidden.length} 项声明能力`"
          :aria-expanded="open"
          :aria-describedby="open ? contentId : undefined"
        >
          <BaseTag size="sm" :type="primary ? 'primary' : 'neutral'">
            +{{ hidden.length }}
          </BaseTag>
        </button>
      </template>
      <div :id="contentId" role="tooltip" class="flex max-w-72 flex-wrap gap-1.5 p-3">
        <BaseTag v-for="[capability, contribution] in hidden" :key="capability" size="sm" :type="primary ? 'primary' : 'neutral'">
          <span :title="contribution.id">{{ pluginCapabilityLabel(capability) }}</span>
        </BaseTag>
      </div>
    </BasePopover>
    <span v-if="!entries.length" class="text-cp-text-quaternary">无</span>
  </div>
</template>
