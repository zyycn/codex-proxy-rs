<script setup lang="ts">
import type { AccountGroup } from '@/api'
import { Search } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { providerDisplayName } from '@/utils/providers'

const props = withDefaults(
  defineProps<{
    groups: AccountGroup[]
    loading?: boolean
    disabled?: boolean
    emptyMode: 'all-accounts' | 'ungrouped' | 'require-selection'
  }>(),
  {
    loading: false,
    disabled: false,
  },
)

const selectedIds = defineModel<string[]>({ required: true })
const search = shallowRef('')
const normalizedSearch = computed(() => search.value.trim().toLocaleLowerCase())
const visibleGroups = computed(() => {
  if (!normalizedSearch.value)
    return props.groups
  return props.groups.filter(group =>
    `${group.name} ${group.description ?? ''}`.toLocaleLowerCase().includes(normalizedSearch.value),
  )
})

const emptyCopy = computed(() => {
  if (props.emptyMode === 'all-accounts') {
    return {
      title: '全部账号权限',
      description: '未选择分组时，该密钥可以使用当前及以后新增的所有账号。',
      tone: 'warning',
    }
  }
  if (props.emptyMode === 'ungrouped') {
    return {
      title: '保持未分组',
      description: '账号暂不加入任何分组，可稍后在账号管理中调整。',
      tone: 'warning',
    }
  }
  if (props.emptyMode === 'require-selection') {
    return {
      title: '请选择分组',
      description: '选中一个或多个分组后才能执行批量操作。',
      tone: 'normal',
    }
  }
  return {
    title: '请选择分组',
    description: '选中一个或多个分组后才能执行批量操作。',
    tone: 'normal',
  }
})

function toggle(groupId: string) {
  if (props.disabled)
    return
  const next = new Set(selectedIds.value)
  if (next.has(groupId))
    next.delete(groupId)
  else
    next.add(groupId)
  selectedIds.value = [...next]
}

function providerSummary(group: AccountGroup) {
  return Object.entries(group.providerCounts)
    .filter(([, count]) => count > 0)
    .map(([provider, count]) => `${providerDisplayName(provider) ?? provider} ${count}`)
    .join(' · ')
}
</script>

<template>
  <div class="grid gap-3">
    <BaseInput
      v-model="search"
      aria-label="搜索账号分组"
      placeholder="搜索分组..."
      :disabled="disabled || loading"
    >
      <template #prefix>
        <Search class="size-4 text-(--cp-text-tertiary)" />
      </template>
    </BaseInput>

    <BaseScrollbar
      v-if="visibleGroups.length > 0"
      max-height="260px"
      view-class="grid gap-2 pr-2"
    >
      <div
        v-for="group in visibleGroups"
        :key="group.id"
        class="flex w-full items-start gap-3 rounded-(--cp-input-radius-base) border-0 px-3 py-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
        :class="selectedIds.includes(group.id)
          ? 'bg-(--cp-info-bg)'
          : 'bg-(--cp-bg-subtle) hover:bg-(--cp-default-bg-hover)'"
      >
        <BaseCheckbox
          :model-value="selectedIds.includes(group.id)"
          :disabled="disabled || loading"
          :label="`选择${group.name}`"
          @update:model-value="toggle(group.id)"
        />
        <span class="min-w-0 flex-1">
          <span class="flex min-w-0 items-center gap-2">
            <strong class="min-w-0 truncate text-[13px] text-(--cp-text-primary)">
              {{ group.name }}
            </strong>
            <span
              v-if="!group.enabled"
              class="shrink-0 rounded-md bg-(--cp-warning-bg) px-1.5 py-1 text-[10px] leading-none font-bold text-(--cp-warning-text)"
            >
              已禁用
            </span>
          </span>
          <span class="mt-1 block text-[11px] font-emphasis text-(--cp-text-secondary)">
            {{ group.memberCount }} 个账号<template v-if="providerSummary(group)"> · {{ providerSummary(group) }}</template>
          </span>
        </span>
      </div>
    </BaseScrollbar>

    <BaseEmpty
      v-else-if="groups.length === 0 && !loading"
      compact
      title="暂无可选分组"
      description="请先在分组管理中创建分组。"
    />
    <BaseEmpty
      v-else-if="!loading"
      compact
      title="没有匹配分组"
      description="请调整搜索关键词。"
    />

    <div
      v-if="selectedIds.length === 0"
      class="rounded-(--cp-input-radius-base) px-3.5 py-3"
      :class="emptyCopy.tone === 'warning'
        ? 'bg-(--cp-warning-bg) text-(--cp-warning-text)'
        : 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'"
    >
      <p class="m-0 text-[12px] font-bold">
        {{ emptyCopy.title }}
      </p>
      <p class="mt-1 mb-0 text-[11px] leading-[1.5] font-emphasis">
        {{ emptyCopy.description }}
      </p>
    </div>
  </div>
</template>
