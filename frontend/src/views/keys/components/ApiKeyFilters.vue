<script setup lang="ts">
import { ZButton, ZInput } from '@codex-proxy/ui'

import { Pencil, Plus, Search, Trash2 } from '@lucide/vue'

import { useI18n } from 'vue-i18n'

defineProps<{
  batchDeleting: boolean
  selectedCount: number
}>()

const emit = defineEmits<{
  create: []
  deleteSelected: []
  editSelected: []
}>()

const { t } = useI18n({ useScope: 'global' })

const search = defineModel<string>('search', { required: true })
</script>

<template>
  <div
    class="flex w-full flex-wrap items-center gap-3"
    role="group"
    :aria-label="t('keys.filters.apiKeyFilteringAndOperations')"
  >
    <div class="min-w-0 flex-1 md:w-96 md:flex-none">
      <ZInput v-model="search" :placeholder="t('keys.filters.searchPlaceholder')" :aria-label="t('keys.filters.searchLabel')" class="w-full">
        <template #prefix>
          <Search class="size-4.5 text-z-text-tertiary" />
        </template>
      </ZInput>
    </div>

    <div class="flex max-w-full shrink-0 flex-wrap items-center justify-end gap-2 md:ml-auto">
      <ZButton v-if="selectedCount > 0" :disabled="batchDeleting" @click="emit('editSelected')">
        <template #icon>
          <Pencil class="size-4" />
        </template>
        {{ t('accounts.filters.editSelected', { count: selectedCount }) }}
      </ZButton>
      <ZButton
        v-if="selectedCount > 0"
        type="danger" variant="plain"
        :disabled="batchDeleting"
        @click="emit('deleteSelected')"
      >
        <template #icon>
          <Trash2 class="size-4" />
        </template>
        {{ t('keys.filters.deleteSelected', { count: selectedCount }) }}
      </ZButton>
      <ZButton type="primary" @click="emit('create')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        {{ t('common.actions.create') }}
      </ZButton>
    </div>
  </div>
</template>
