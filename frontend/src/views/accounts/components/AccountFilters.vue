<script setup lang="ts">
import type { AccountGroup } from '@/api'
import { Download, Pencil, Search, Trash2, Upload } from '@lucide/vue'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import ProviderFilterSegmented from '@/components/ProviderFilterSegmented.vue'
import { accountStatusFilterOptions } from '../constants'

const props = defineProps<{
  selectedCount: number
  batchDeleting: boolean
  exportingAccounts: boolean
  groups: AccountGroup[]
  groupsLoading: boolean
}>()

const emit = defineEmits<{
  deleteSelected: []
  exportSelected: []
  create: []
  editSelected: []
}>()

const search = defineModel<string>('search', { required: true })
const status = defineModel<string>('status', { required: true })
const provider = defineModel<string>('provider', { required: true })
const group = defineModel<string>('group', { required: true })
const groupOptions = computed(() => [
  { label: '全部分组', value: '' },
  { label: '未分组账号', value: 'ungrouped' },
  ...props.groups.map(group => ({
    label: group.enabled ? group.name : `${group.name}（已禁用）`,
    value: group.id,
  })),
])
</script>

<template>
  <div
    class="flex w-full flex-col gap-3 md:flex-row md:flex-wrap md:items-center"
    role="group"
    aria-label="账号筛选与操作"
  >
    <div class="flex min-w-0 flex-wrap items-center gap-2 md:flex-none md:gap-3">
      <BaseInput
        v-model="search"
        placeholder="搜索账号"
        class="min-w-0 flex-1 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-80 md:flex-none"
      >
        <template #prefix>
          <Search class="size-4.5 text-cp-tertiary" />
        </template>
      </BaseInput>

      <BaseSelect
        v-model="status"
        :options="accountStatusFilterOptions"
        aria-label="按账号状态筛选"
        class="w-34 shrink-0 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-40"
      />

      <BaseSelect
        v-model="group"
        :options="groupOptions"
        :disabled="groupsLoading"
        aria-label="按账号分组筛选"
        class="w-34 shrink-0 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-40"
      />

      <ProviderFilterSegmented
        v-model="provider"
        class="w-31 shrink-0"
      />
    </div>

    <div
      class="grid w-full grid-cols-2 gap-2 md:flex md:w-auto md:flex-wrap md:shrink-0 md:self-end md:items-center md:justify-end md:ml-auto"
    >
      <BaseButton
        v-if="selectedCount > 0"
        variant="secondary"
        class="w-full whitespace-nowrap md:w-auto"
        @click="emit('editSelected')"
      >
        <Pencil class="size-4 text-cp-info" />
        批量编辑账号
      </BaseButton>
      <BaseButton
        v-if="selectedCount > 0"
        variant="destructive"
        class="w-full whitespace-nowrap md:w-auto"
        :disabled="batchDeleting"
        @click="emit('deleteSelected')"
      >
        <Trash2 class="size-4" />
        删除选中 ({{ selectedCount }})
      </BaseButton>
      <BaseButton
        v-if="selectedCount > 0"
        variant="secondary"
        class="w-full whitespace-nowrap md:w-auto"
        :loading="exportingAccounts"
        @click="emit('exportSelected')"
      >
        <template #icon>
          <Download class="size-4" />
        </template>
        导出选中 ({{ selectedCount }})
      </BaseButton>
      <BaseButton
        variant="primary"
        class="whitespace-nowrap md:w-auto"
        :class="selectedCount > 0 ? 'col-span-2 w-full' : 'col-span-2 justify-self-end'"
        @click="emit('create')"
      >
        <Upload class="size-4" />
        导入账号
      </BaseButton>
    </div>
  </div>
</template>
