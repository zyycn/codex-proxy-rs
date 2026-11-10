<script setup lang="ts">
import { ZIconButton, ZPopover } from '@codex-proxy/ui'

defineOptions({ inheritAttrs: false })
defineProps<{ label: string }>()
</script>

<template>
  <ZPopover trigger="hover" role="tooltip" placement="top" :hover-delay="500" :hide-after="0">
    <template #reference="{ close }">
      <ZIconButton v-bind="$attrs" size="small" :aria-label="label" @click="close">
        <template v-if="$slots.loading" #loading>
          <slot name="loading" />
        </template>
        <slot />
      </ZIconButton>
    </template>
    <div class="px-2.5 py-1.5 text-z-xs font-emphasis text-z-text">
      {{ label }}
    </div>
  </ZPopover>
</template>
