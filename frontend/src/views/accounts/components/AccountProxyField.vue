<script setup lang="ts">
import type { OutboundProxy } from '@/api'
import { computed } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

const props = withDefaults(
  defineProps<{
    endpoint?: string | null
    disabled?: boolean
    preserve?: boolean
    error?: string
    proxies?: OutboundProxy[]
    proxiesLoading?: boolean
  }>(),
  { preserve: true, proxies: () => [], proxiesLoading: false },
)
const mode = defineModel<string>('mode', { required: true })
const url = defineModel<string>('url', { required: true })
const proxyId = defineModel<string>('proxyId', { required: false, default: '' })
const options = computed(() => [
  ...(props.preserve ? [{ label: '保持当前', value: 'preserve' }] : []),
  { label: '直连', value: 'direct' },
  ...(props.proxies.length > 0 ? [{ label: '从 IP 池选择', value: 'pool' }] : []),
  { label: '手动填写代理', value: 'proxy' },
])
const poolOptions = computed(() =>
  props.proxies.map(proxy => ({
    label: `${proxy.name}（${proxy.endpoint}）`,
    value: proxy.id,
  })),
)
</script>

<template>
  <div class="grid gap-5">
    <BaseFormItem label="出站隧道">
      <BaseSelect
        v-model="mode"
        class="w-full"
        :options="options"
        :disabled="disabled"
        aria-label="出站隧道"
      />
      <p v-if="endpoint && mode === 'preserve'" class="mt-2 mb-0 font-mono text-xs break-all text-cp-text-quaternary">
        {{ endpoint }}
      </p>
    </BaseFormItem>
    <BaseFormItem v-if="mode === 'pool'" label="选择代理" required :error="error">
      <BaseSelect
        v-model="proxyId"
        class="w-full"
        :options="poolOptions"
        :disabled="disabled || proxiesLoading"
        placeholder="选择 IP 池中的代理"
        aria-label="选择代理"
      />
      <p class="mt-2 mb-0 text-xs text-cp-text-quaternary">
        代理列表在「IP 管理」页面维护。
      </p>
    </BaseFormItem>
    <BaseFormItem v-if="mode === 'proxy'" label="代理 URL" required :error="error">
      <BaseInput
        v-model="url"
        type="password"
        autocomplete="new-password"
        aria-label="代理 URL"
        placeholder="例如：http://127.0.0.1:7890"
        :disabled="disabled"
      />
    </BaseFormItem>
  </div>
</template>
