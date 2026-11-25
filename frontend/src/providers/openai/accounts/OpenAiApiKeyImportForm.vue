<script setup lang="ts">
import { ZForm } from '@codex-proxy/ui'
import { onActivated, ref, useTemplateRef } from 'vue'
import ApiKeyFields from './ApiKeyFields.vue'
import { emptyApiKeyAccountForm } from './connection'
import { apiKeyDocument } from './import'

const props = defineProps<{ disabled: boolean }>()
const emit = defineEmits<{ create: [document: ReturnType<typeof apiKeyDocument>] }>()
const connection = ref(emptyApiKeyAccountForm())
const form = useTemplateRef<InstanceType<typeof ZForm>>('form')

onActivated(() => form.value?.clearValidate())

async function submit() {
  if (!props.disabled && await form.value?.validate().catch(() => false))
    emit('create', apiKeyDocument(connection.value))
}
</script>

<template>
  <ZForm ref="form" :model="{ connection }" scroll-to-error focus-on-error novalidate @submit="submit">
    <ApiKeyFields v-model="connection" :disabled="disabled" />
  </ZForm>
</template>
