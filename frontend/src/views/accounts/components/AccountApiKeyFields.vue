<script setup lang="ts">
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

defineProps<{ disabled?: boolean, editing?: boolean }>()
const form = defineModel<ApiKeyAccountForm>({ required: true })
const transportOptions = [
  { label: 'HTTP / SSE', value: 'http' },
  { label: 'WS 优先', value: 'prefer_websocket' },
]
</script>

<template>
  <div class="grid gap-4">
    <BaseFormItem v-if="!editing" label="账号名称" required>
      <BaseInput v-model="form.name" placeholder="输入上游名称" :disabled="disabled" />
    </BaseFormItem>
    <BaseFormItem label="Base URL" required>
      <template #extra>
        <span class="text-cp-xs text-cp-text-quaternary" title="填写 API 前缀，接口路径会自动追加">API 前缀</span>
      </template>
      <BaseInput v-model="form.base_url" type="url" placeholder="https://api.example.com/v1" :disabled="disabled" autocomplete="off" />
    </BaseFormItem>
    <BaseFormItem label="API Key" :required="!editing">
      <BaseInput v-model="form.apiKey" type="password" :placeholder="editing ? '留空保留当前密钥' : '输入上游密钥'" :disabled="disabled" autocomplete="new-password" />
    </BaseFormItem>
    <BaseFormItem label="传输方式">
      <BaseSegmented v-model="form.transport" label="传输方式" :options="transportOptions" :disabled="disabled" title="WS 优先需要上游支持 Responses WebSocket" />
    </BaseFormItem>
  </div>
</template>
