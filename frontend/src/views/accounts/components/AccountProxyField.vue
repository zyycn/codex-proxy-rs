<script setup lang="ts">
import { computed } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

const props = withDefaults(defineProps<{ accountId?: string, endpoint?: string | null, disabled?: boolean, preserve?: boolean, error?: string }>(), { preserve: true })
const mode = defineModel<string>('mode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading } = useProxyCatalog()
// 目录不再携带账号明细；脱敏地址无法证明绑定关系，保持项只展示账号已有地址。
const options = computed(() => [
  ...(props.preserve
    ? [{
        label: '保持当前',
        value: 'preserve',
        description: props.endpoint || (props.accountId ? '直连' : '保留各账号的连接设置'),
      }]
    : []),
  { label: '直连', value: 'direct', description: '不使用代理' },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name}${proxy.lastTest?.success ? '' : proxy.lastTest ? '（测试失败）' : '（未测试）'}`,
    value: proxy.id,
    description: proxy.endpoint,
    disabled: proxy.lastTest?.success !== true,
  })),
])
const selection = computed({
  get: () => mode.value === 'proxy' ? proxyId.value : mode.value,
  set: (value: string) => {
    const nextMode = value === 'preserve' || value === 'direct' ? value : 'proxy'
    mode.value = nextMode
    proxyId.value = nextMode === 'proxy' ? value : ''
  },
})
</script>

<template>
  <BaseFormItem label="出站隧道" :error="error">
    <BaseSelect v-model="selection" class="w-full" :options="options" :disabled="disabled || loading" aria-label="出站隧道" />
  </BaseFormItem>
</template>
