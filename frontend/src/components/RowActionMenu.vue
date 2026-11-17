<script setup lang="ts">
import { ZDropdown, ZIconButton } from '@codex-proxy/ui'
import { MoreHorizontal } from '@lucide/vue'

defineProps<{ label: string, menuClass: string, disabled?: boolean, loading?: boolean }>()
</script>

<template>
  <ZDropdown :menu-class="menuClass">
    <template #reference="{ trigger }">
      <ZIconButton v-bind="trigger" size="small" :aria-label="label" :disabled="disabled" :loading="loading" :class="trigger['aria-expanded'] ? 'bg-z-bg-text-active text-z-text' : undefined">
        <MoreHorizontal class="size-4" />
      </ZIconButton>
    </template>
    <slot />
  </ZDropdown>
</template>
