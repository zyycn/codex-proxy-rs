<script setup lang="ts">
import type { PluginArtifact } from '@/api'
import { Package } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import { pluginArtifactIconPath } from '@/api'
import { useThemeStore } from '@/stores/modules/theme'

const props = defineProps<{
  artifact: PluginArtifact
}>()

const theme = useThemeStore()
const failedSource = shallowRef('')
const source = computed(() => props.artifact.metadata.icon
  ? pluginArtifactIconPath(props.artifact.metadata.sha256, theme.effectiveTheme)
  : '')
</script>

<template>
  <span class="inline-flex shrink-0 items-center justify-center overflow-hidden bg-cp-fill-alter text-cp-text-secondary" aria-hidden="true">
    <img
      v-if="source && source !== failedSource"
      :key="source"
      :src="source"
      alt=""
      class="size-full object-contain p-1"
      loading="lazy"
      decoding="async"
      @error="failedSource = source"
    >
    <Package v-else class="size-3/5" :stroke-width="1.6" />
  </span>
</template>
