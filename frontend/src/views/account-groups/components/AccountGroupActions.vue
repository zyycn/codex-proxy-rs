<script setup lang="ts">
import type { AccountGroup } from '@/api'
import { Pencil, Power, Trash2 } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'

defineProps<{
  group: AccountGroup
  updatingStatus: boolean
  deleting: boolean
}>()
const emit = defineEmits<{
  edit: [group: AccountGroup]
  toggle: [group: AccountGroup]
  delete: [group: AccountGroup]
}>()
</script>

<template>
  <div class="flex items-center gap-1">
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="编辑分组"
      @click.stop="emit('edit', group)"
    >
      <Pencil class="size-3.5 text-cp-info" />
    </BaseIconButton>
    <BaseIconButton
      variant="ghost"
      size="sm"
      :label="group.enabled ? '禁用分组' : '启用分组'"
      :loading="updatingStatus"
      @click.stop="emit('toggle', group)"
    >
      <Power
        class="size-3.5"
        :class="group.enabled ? 'text-cp-warning' : 'text-cp-success'"
      />
    </BaseIconButton>
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="删除分组"
      :disabled="deleting"
      @click.stop="emit('delete', group)"
    >
      <Trash2 class="size-3.5 text-cp-danger" />
    </BaseIconButton>
  </div>
</template>
