<script setup lang="ts">
import type { PromptRewriteRule } from '@/api'
import { ZButton, ZCard, ZEmpty, ZIconButton } from '@codex-proxy/ui'
import { LayoutGrid, Pencil, Plus, Trash2 } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { formatProviderLabel } from '@/providers/catalog'
import RuleEditor from './RuleEditor.vue'

const props = defineProps<{ disabled: boolean, ready: boolean }>()
const rules = defineModel<PromptRewriteRule[]>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const editorOpen = shallowRef(false)
const editingIndex = shallowRef<number | null>(null)
const selectedRule = computed(() => editingIndex.value === null
  ? undefined
  : rules.value[editingIndex.value])
const rows = computed(() => rules.value.map(rule => ({
  rule,
  title: rule.name,
  providers: rule.providers.length
    ? rule.providers.map(provider => formatProviderLabel(provider)).join(' + ')
    : t('common.providers.all'),
})))

function edit(index: number | null) {
  editingIndex.value = index
  editorOpen.value = true
}

function apply(rule: PromptRewriteRule) {
  if (props.disabled)
    return
  rules.value = editingIndex.value === null
    ? [...rules.value, rule]
    : rules.value.map((item, index) => index === editingIndex.value
        ? rule
        : item)
}
</script>

<template>
  <ZCard :title="t('settings.promptRewrite.title')" :description="t('settings.promptRewrite.description')">
    <template #actions>
      <ZButton :disabled="disabled" @click="edit(null)">
        <template #icon>
          <Plus class="size-4" />
        </template>
        {{ t('settings.promptRewrite.addRule') }}
      </ZButton>
    </template>
    <template #body>
      <ZEmpty v-if="ready && !rules.length" :title="t('settings.promptRewrite.empty')" size="small" surface="none" role="status" class="flex-1 content-center py-3!" />
      <ul v-if="rules.length" class="m-0 grid list-none gap-2 p-0">
        <li v-for="(row, index) in rows" :key="index" class="flex min-w-0 items-center gap-3 rounded-z-lg bg-z-fill-quaternary px-3 py-2">
          <div class="grid min-w-0 flex-1 gap-1 sm:grid-cols-[10rem_minmax(0,1fr)] sm:items-center sm:gap-4">
            <span class="flex min-w-0 items-center gap-2 text-z-sm text-z-text-secondary" :title="row.providers">
              <span class="flex shrink-0 items-center gap-1" aria-hidden="true">
                <ProviderIconGroup v-for="provider in row.rule.providers" :key="provider" :provider="provider" size="sm" />
                <LayoutGrid v-if="!row.rule.providers.length" class="size-4" />
              </span>
              <span class="truncate">{{ row.providers }}</span>
            </span>
            <span class="truncate text-z-sm font-emphasis text-z-text" :title="row.title">{{ row.title }}</span>
          </div>
          <div class="flex shrink-0 items-center gap-1">
            <ZIconButton :disabled="disabled" :aria-label="t('settings.promptRewrite.editNamedRule', { name: row.title })" @click="edit(index)">
              <Pencil class="size-4" />
            </ZIconButton>
            <ZIconButton :disabled="disabled" :aria-label="t('settings.promptRewrite.deleteRule', { name: row.title })" @click="rules = rules.filter((_, i) => i !== index)">
              <Trash2 class="size-4 text-z-error" />
            </ZIconButton>
          </div>
        </li>
      </ul>
      <RuleEditor v-model="editorOpen" :rule="selectedRule" :disabled="disabled" @confirm="apply" />
    </template>
  </ZCard>
</template>
