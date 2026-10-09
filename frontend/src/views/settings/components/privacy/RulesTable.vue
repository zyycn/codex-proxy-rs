<script setup lang="ts">
import type { PrivacyRule } from '@/api/modules/settings/privacy'
import { BaseIconButton, BaseMenuItem, BasePopover, BaseSwitch, BaseTable, defineTableColumns } from '@codex-proxy/ui'
import { ArrowDown, ArrowUp, Copy, MoreHorizontal, Pencil, Trash2 } from '@lucide/vue'
import { shallowRef } from 'vue'
import { actions, scopes } from './presets'

defineProps<{ rules: PrivacyRule[], disabled: boolean }>()
const emit = defineEmits<{
  edit: [rule: PrivacyRule]
  toggle: [rule: PrivacyRule, enabled: boolean]
  move: [index: number, step: number]
  duplicate: [rule: PrivacyRule]
  remove: [rule: PrivacyRule]
}>()
const activeMenu = shallowRef('')
const columns = defineTableColumns<PrivacyRule>([
  { key: 'enabled', label: '启用', kind: 'status', size: 'sm', fixedWidth: true },
  { key: 'name', label: '规则 / 字段', kind: 'identity', size: '3xl' },
  { key: 'scope', label: '作用范围', kind: 'meta', size: 'lg' },
  { key: 'action', label: '处理方式', kind: 'meta', size: 'md' },
  { key: 'actions', label: '', kind: 'actions', size: 'sm', fixedWidth: true },
])
</script>

<template>
  <BaseTable :columns="columns" :rows="rules" density="compact" empty-text="暂无规则，添加一条或从预设开始">
    <template #enabled="{ row }">
      <div class="flex items-center justify-center">
        <BaseSwitch :disabled="disabled" :model-value="row.enabled" :label="`启用规则：${row.name}`" @update:model-value="emit('toggle', row, $event)" />
      </div>
    </template>
    <template #name="{ row }">
      <div class="grid min-w-0 gap-0.5 py-1.5">
        <span class="truncate font-emphasis text-cp-text">{{ row.name }}</span>
        <span class="truncate font-mono text-cp-xs text-cp-text-quaternary" :title="row.selector">{{ row.selector }}</span>
      </div>
    </template>
    <template #scope="{ row }">
      {{ scopes.find(item => item.value === row.scope)?.label }}
    </template>
    <template #action="{ row }">
      {{ actions.find(item => item.value === row.action)?.label }}
    </template>
    <template #actions="{ row }">
      <div class="flex items-center justify-end gap-1">
        <BaseIconButton size="sm" :disabled="disabled" variant="ghost" :label="`编辑${row.name}`" @click="emit('edit', row)">
          <Pencil class="size-3.5" />
        </BaseIconButton>
        <BasePopover :model-value="activeMenu === row.id" @update:model-value="activeMenu = $event ? row.id : ''">
          <template #trigger>
            <BaseIconButton size="sm" :disabled="disabled" :label="`${row.name}的更多操作`" variant="ghost">
              <MoreHorizontal class="size-3.5" />
            </BaseIconButton>
          </template>
          <div class="w-40 p-1.5">
            <BaseMenuItem :disabled="rules[0]?.id === row.id" @click="emit('move', rules.indexOf(row), -1); activeMenu = ''">
              <template #icon>
                <ArrowUp class="size-3.5" />
              </template>上移
            </BaseMenuItem>
            <BaseMenuItem :disabled="rules.at(-1)?.id === row.id" @click="emit('move', rules.indexOf(row), 1); activeMenu = ''">
              <template #icon>
                <ArrowDown class="size-3.5" />
              </template>下移
            </BaseMenuItem>
            <BaseMenuItem @click="emit('duplicate', row); activeMenu = ''">
              <template #icon>
                <Copy class="size-3.5" />
              </template>复制
            </BaseMenuItem>
            <BaseMenuItem tone="destructive" @click="emit('remove', row); activeMenu = ''">
              <template #icon>
                <Trash2 class="size-3.5" />
              </template>删除规则
            </BaseMenuItem>
          </div>
        </BasePopover>
      </div>
    </template>
  </BaseTable>
</template>
