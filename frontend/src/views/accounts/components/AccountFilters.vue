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
    class="flex w-full flex-col gap-3 xl:flex-row xl:flex-wrap xl:items-center"
    role="group"
    aria-label="账号筛选与操作"
  >
    <div
      class="grid w-full min-w-0 grid-cols-2 items-center gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_7.75rem] xl:flex xl:w-auto xl:flex-none xl:flex-wrap xl:gap-3"
    >
      <BaseInput
        v-model="search"
        placeholder="搜索账号"
        class="col-span-2 min-w-0 sm:col-span-3 xl:w-80 xl:flex-none"
      >
        <template #prefix>
          <Search class="size-4.5 text-cp-text-tertiary" />
        </template>
      </BaseInput>

      <BaseSelect
        v-model="status"
        :options="accountStatusFilterOptions"
        aria-label="按账号状态筛选"
        class="w-full min-w-0 xl:w-40 xl:shrink-0"
      />

      <BaseSelect
        v-model="group"
        :options="groupOptions"
        :disabled="groupsLoading"
        aria-label="按账号分组筛选"
        class="w-full min-w-0 xl:w-40 xl:shrink-0"
      />

      <ProviderFilterSegmented
        v-model="provider"
        class="col-span-2 w-full sm:col-span-1 xl:w-31 xl:shrink-0"
      />
    </div>

    <div
      class="grid w-full grid-cols-2 gap-2 xl:flex xl:w-auto xl:flex-wrap xl:shrink-0 xl:self-end xl:items-center xl:justify-end xl:ml-auto"
    >
      <BaseButton
        v-if="selectedCount > 0"
        variant="secondary"
        class="w-full whitespace-nowrap xl:w-auto"
        @click="emit('editSelected')"
      >
        <Pencil class="size-4 text-cp-link" />
        批量编辑账号
      </BaseButton>
      <BaseButton
        v-if="selectedCount > 0"
        variant="destructive"
        class="w-full whitespace-nowrap xl:w-auto"
        :disabled="batchDeleting"
        @click="emit('deleteSelected')"
      >
        <Trash2 class="size-4" />
        删除选中 ({{ selectedCount }})
      </BaseButton>
      <BaseButton
        v-if="selectedCount > 0"
        variant="secondary"
        class="w-full whitespace-nowrap xl:w-auto"
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
        class="whitespace-nowrap xl:w-auto"
        :class="selectedCount > 0 ? 'col-span-2 w-full' : 'col-span-2 justify-self-end'"
        @click="emit('create')"
      >
        <Upload class="size-4" />
        导入账号
      </BaseButton>
    </div>
  </div>
</template>
