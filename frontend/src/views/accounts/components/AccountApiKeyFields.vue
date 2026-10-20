<script setup lang="ts">
import type { ApiKeyBalanceQuery } from '@/api'
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import { ZFormItem, ZInput, ZSegmented } from '@codex-proxy/ui'
import { computed } from 'vue'
import { balanceQueryOptions, kimiBalanceQueryOptions } from '../utils/upstreamApiKey'

defineProps<{ disabled?: boolean, editing?: boolean }>()
const form = defineModel<ApiKeyAccountForm>({ required: true })
const transportOptions = [
  { label: 'HTTP / SSE', value: 'http' },
  { label: 'WS 优先', value: 'prefer_websocket' },
]
const kimiPlanOptions = [
  { label: '按量付费', value: 'payg' },
  { label: '编程订阅', value: 'coding' },
]
const kimiRegionOptions = [
  { label: '国内', value: 'cn' },
  { label: '海外', value: 'intl' },
]
const kimiSelection = computed(() => kimiBalanceQueryOptions.find(option => option.value === form.value.balance_query))
const balanceQuery = computed({
  get: () => kimiSelection.value ? 'kimi' : form.value.balance_query,
  set: (value: ApiKeyBalanceQuery) => { form.value.balance_query = value },
})
const kimiPlan = computed<'payg' | 'coding'>({
  get: () => kimiSelection.value?.plan ?? 'payg',
  set: plan => selectKimiQuery(plan, kimiRegion.value),
})
const kimiRegion = computed<'cn' | 'intl'>({
  get: () => kimiSelection.value?.region ?? 'cn',
  set: region => selectKimiQuery(kimiPlan.value, region),
})

function selectKimiQuery(plan: 'payg' | 'coding', region: 'cn' | 'intl') {
  form.value.balance_query = kimiBalanceQueryOptions.find(option => option.plan === plan && option.region === region)!.value
}
</script>

<template>
  <div class="grid gap-4">
    <ZFormItem v-if="!editing" label="账号名称" required>
      <ZInput v-model="form.name" placeholder="输入上游名称" :disabled="disabled" />
    </ZFormItem>
    <ZFormItem label="Base URL" required>
      <template #extra>
        <span class="text-cp-xs text-cp-text-quaternary" title="填写 API 前缀，接口路径会自动追加">API 前缀</span>
      </template>
      <ZInput v-model="form.base_url" type="url" placeholder="https://api.example.com/v1" :disabled="disabled" autocomplete="off" />
    </ZFormItem>
    <ZFormItem label="API Key" :required="!editing">
      <ZInput v-model="form.apiKey" type="password" :placeholder="editing ? '留空保留当前密钥' : '输入上游密钥'" :disabled="disabled" autocomplete="new-password" />
    </ZFormItem>
    <ZFormItem label="传输方式">
      <ZSegmented v-model="form.transport" aria-label="传输方式" :options="transportOptions" :disabled="disabled" title="WS 优先需要上游支持 Responses WebSocket" />
    </ZFormItem>
    <ZFormItem label="用量查询">
      <ZSegmented v-model="balanceQuery" aria-label="用量查询类型" :options="balanceQueryOptions" :disabled="disabled" />
    </ZFormItem>
    <template v-if="kimiSelection">
      <ZFormItem label="Kimi 套餐">
        <ZSegmented v-model="kimiPlan" aria-label="Kimi 套餐" :options="kimiPlanOptions" :disabled="disabled" />
      </ZFormItem>
      <ZFormItem label="Kimi 地区">
        <ZSegmented v-model="kimiRegion" aria-label="Kimi 地区" :options="kimiRegionOptions" :disabled="disabled" />
      </ZFormItem>
    </template>
  </div>
</template>
