<script setup lang="ts">
import type { AccountGroupRef } from '@/api'
import { computed } from 'vue'

const props = defineProps<{
  groups: AccountGroupRef[]
}>()

const fullNames = computed(() =>
  props.groups.map(group => (group.enabled ? group.name : `${group.name}（已禁用）`)).join('、'),
)
</script>

<template>
  <div
    v-if="groups.length > 0"
    class="flex min-w-0 flex-wrap items-center gap-1.5"
    :title="fullNames"
    :aria-label="fullNames"
  >
    <span
      v-for="group in groups"
      :key="group.id"
      class="size-4 shrink-0 rounded-sm"
      :style="{ backgroundColor: group.color }"
      :title="group.name"
    />
  </div>
  <span v-else class="text-xs text-cp-text-quaternary">—</span>
</template>
