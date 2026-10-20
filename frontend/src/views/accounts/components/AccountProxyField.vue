<script setup lang="ts">
import { ZFormItem, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

const props = withDefaults(defineProps<{ accountId?: string, currentProxyId?: string | null, endpoint?: string | null, disabled?: boolean, preserve?: boolean, error?: string }>(), { preserve: true })
const mode = defineModel<string>('mode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading } = useProxyCatalog()
// 按绑定 ID 识别代理，避免相同脱敏地址下的不同认证配置被误认
const currentSelection = computed(() => props.currentProxyId || (props.endpoint ? 'preserve' : 'direct'))
const options = computed(() => [
  ...(props.preserve && !props.accountId
    ? [{
        label: '保持当前',
        value: 'preserve',
        description: '保留各账号的连接设置',
      }]
    : []),
  ...(props.preserve && props.accountId && props.endpoint && !proxies.value.some(proxy => proxy.id === props.currentProxyId)
    ? [{ label: props.endpoint, value: currentSelection.value }]
    : []),
  { label: '直连', value: 'direct', description: '不使用代理' },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name.trim() || proxy.endpoint}${!proxy.enabled ? '（已禁用）' : proxy.lastTest?.success ? '' : proxy.lastTest ? '（测试失败）' : '（未测试）'}`,
    value: proxy.id,
    description: proxy.name.trim() ? proxy.endpoint : undefined,
    disabled: !proxy.enabled,
  })),
])
const selection = computed({
  get: () => mode.value === 'proxy'
    ? proxyId.value
    : mode.value === 'preserve' && props.accountId ? currentSelection.value : mode.value,
  set: (value: string) => {
    // 显示已绑定代理，未改选择时仍省略写入，保留禁用代理及仅地址配置
    const nextMode = props.preserve && props.accountId && value === currentSelection.value
      ? 'preserve'
      : value === 'preserve' || value === 'direct' ? value : 'proxy'
    mode.value = nextMode
    proxyId.value = nextMode === 'proxy' ? value : ''
  },
})
</script>

<template>
  <ZFormItem label="出站隧道" :error="error">
    <ZSelect v-model="selection" class="w-full" :options="options" :disabled="disabled || loading" aria-label="出站隧道" />
  </ZFormItem>
</template>
