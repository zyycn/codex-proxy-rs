<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  apiKey: any
  editing: boolean
  editingValue: string
  saving: boolean
}>()

const emit = defineEmits<{
  startEdit: [apiKey: any]
  updateEdit: [value: string]
  submitEdit: [apiKeyId: string]
  cancelEdit: []
}>()

const editValue = computed({
  get: () => props.editingValue,
  set: (value: string) => emit('updateEdit', value),
})
</script>

<template>
  <div class="flex flex-col gap-0.5">
    <span class="text-[13px] font-bold text-(--cp-text-primary)">
      {{ apiKey.name }}
    </span>
    <div v-if="editing" class="flex items-center gap-2">
      <input
        v-model="editValue"
        type="text"
        class="h-(--cp-input-height-inline) min-w-34 rounded-(--cp-input-radius-small) border-0 bg-(--cp-input-soft-bg) px-2.5 text-[13px] font-[650] text-(--cp-text-primary) shadow-(--cp-shadow-input) outline-none transition placeholder:text-(--cp-text-muted) focus:bg-(--cp-input-soft-bg-focus) focus:shadow-(--cp-shadow-input-focus)"
        :disabled="saving"
        @keyup.enter="emit('submitEdit', apiKey.id)"
        @keyup.escape="emit('cancelEdit')"
      />
      <button
        class="text-[12px] text-(--cp-accent-primary) hover:underline disabled:cursor-not-allowed disabled:text-(--cp-disabled-text) disabled:no-underline"
        :disabled="saving"
        @click="emit('submitEdit', apiKey.id)"
      >
        {{ saving ? '保存中' : '保存' }}
      </button>
      <button
        class="text-[12px] text-(--cp-text-tertiary) hover:underline disabled:cursor-not-allowed disabled:text-(--cp-disabled-text) disabled:no-underline"
        :disabled="saving"
        @click="emit('cancelEdit')"
      >
        取消
      </button>
    </div>
    <button
      v-else-if="apiKey.label"
      class="text-left text-[12px] font-[650] text-(--cp-text-tertiary) hover:text-(--cp-info-text)"
      @click="emit('startEdit', apiKey)"
    >
      {{ apiKey.label }}
    </button>
  </div>
</template>
