<script setup lang="ts">
import { ZButton, ZCheckbox, ZIconButton, ZPopover, ZScrollbar } from '@codex-proxy/ui'
import { BrushCleaning } from '@lucide/vue'
import { useEventListener } from '@vueuse/core'
import { computed, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  models: string[]
  modelCount: number
  disabled: boolean
}>()
const emit = defineEmits<{
  remove: [ids: string[]]
  closed: []
}>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(false)
const content = useTemplateRef<HTMLDivElement>('content')
useEventListener(content, 'keydown', handleKeydown)
const selected = ref<string[]>([])
const selectedModels = computed(() => props.models.filter(id => selected.value.includes(id)))
const wouldEmpty = computed(() => selectedModels.value.length === props.modelCount)

watch(open, () => {
  selected.value = []
})
watch([
  () => props.models,
  () => props.disabled,
], () => {
  open.value = false
})

function select(id: string, checked: boolean) {
  selected.value = checked
    ? [...selected.value, id]
    : selected.value.filter(value => value !== id)
}

function remove() {
  if (props.disabled || !selectedModels.value.length || wouldEmpty.value)
    return
  emit('remove', selectedModels.value)
  open.value = false
}

function handleKeydown(event: KeyboardEvent) {
  if (!open.value)
    return
  if (event.key === 'Escape') {
    // 先关闭清理浮层，避免丢弃外层账号表单的未保存修改
    event.preventDefault()
    event.stopPropagation()
    open.value = false
  }
  if (event.key === 'Tab' && content.value?.contains(event.target as Node)) {
    // 清理是独立的选择步骤，焦点不能穿过传送到 body 的面板进入外层表单
    const controls = [
      ...content.value.querySelectorAll<HTMLElement>('input:not(:disabled), button:not(:disabled), [tabindex="0"]'),
    ]
    const first = controls[0]
    const last = controls.at(-1)
    if (event.shiftKey
      ? document.activeElement === first
      : document.activeElement === last) {
      event.preventDefault()
      ;(event.shiftKey ? last : first)?.focus()
    }
  }
}

function focusSelection() {
  content.value?.querySelector<HTMLInputElement>('input')?.focus()
}
</script>

<template>
  <ZPopover v-model="open" placement="bottom-end" :disabled="disabled" @opened="focusSelection" @closed="emit('closed')">
    <template #reference="{ id }">
      <ZIconButton
        variant="plain"
        :aria-label="t('accounts.modelAccess.cleanupModels')"
        :aria-expanded="open"
        :aria-controls="open ? id : undefined"
        aria-haspopup="dialog"
        :pressed="open"
        :disabled="disabled"
        @keydown="handleKeydown"
      >
        <BrushCleaning class="size-3.5" aria-hidden="true" />
      </ZIconButton>
    </template>
    <div ref="content" class="flex max-h-[calc(100dvh-1.5rem)] w-72 max-w-[calc(100vw-2rem)] flex-col gap-3 overflow-y-auto p-3" role="dialog" :aria-label="t('accounts.modelAccess.cleanupModels')" tabindex="-1">
      <p class="m-0 shrink-0 text-z-sm font-emphasis text-z-text">
        {{ t('accounts.modelAccess.cleanupModels') }}
      </p>
      <ZScrollbar max-height="12rem" class="flex min-h-0 flex-col">
        <div class="grid gap-2" role="group" :aria-label="t('accounts.modelAccess.cleanupSelection')">
          <ZCheckbox
            v-for="id in models"
            :key="id"
            :model-value="selected.includes(id)"
            :label="id"
            :title="id"
            class="min-w-0"
            @update:model-value="select(id, $event)"
          >
            <template #label>
              <span class="block break-all">{{ id }}</span>
            </template>
          </ZCheckbox>
        </div>
      </ZScrollbar>
      <p v-if="wouldEmpty" class="m-0 shrink-0 text-z-xs text-z-warning-text" role="status">
        {{ t('accounts.modelAccess.cleanupKeepOne') }}
      </p>
      <div class="flex shrink-0 justify-end gap-2">
        <ZButton size="small" @click="open = false">
          {{ t('common.actions.cancel') }}
        </ZButton>
        <ZButton size="small" type="primary" :disabled="disabled || !selectedModels.length || wouldEmpty" @click="remove">
          {{ selectedModels.length ? t('accounts.modelAccess.cleanupSelected', { count: selectedModels.length }) : t('accounts.modelAccess.cleanupRemove') }}
        </ZButton>
      </div>
    </div>
  </ZPopover>
</template>
