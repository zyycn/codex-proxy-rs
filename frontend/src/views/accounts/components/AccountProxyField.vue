<script setup lang="ts">
import { ZButton, ZFormItem, ZSelect } from '@codex-proxy/ui'
import { Wifi } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useProxyCatalog } from '@/composables/useProxyCatalog'
import { useAccountProxyTest } from '../composables/useAccountProxyTest'

const props = withDefaults(defineProps<{ accountId?: string, currentProxyId?: string | null, endpoint?: string | null, disabled?: boolean, preserve?: boolean, formProp: string }>(), { preserve: true })

const { t } = useI18n({ useScope: 'global' })

const mode = defineModel<string>('mode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading } = useProxyCatalog()
// 按绑定 ID 识别代理，避免相同脱敏地址下的不同认证配置被误认
const currentSelection = computed(() => props.currentProxyId || (props.endpoint ? 'preserve' : 'direct'))
const options = computed(() => [
  ...(props.preserve && !props.accountId
    ? [
        {
          label: t('accounts.proxy.keepCurrent'),
          value: 'preserve',
          description: t('accounts.proxy.keepExisting'),
        },
      ]
    : []),
  ...(props.preserve && props.accountId && props.endpoint && !proxies.value.some(proxy => proxy.id === props.currentProxyId)
    ? [
        { label: props.endpoint, value: currentSelection.value },
      ]
    : []),
  {
    label: t('accounts.proxy.directConnection'),
    value: 'direct',
    description: t('accounts.proxy.notUsingProxy'),
  },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name.trim() || proxy.endpoint}${!proxy.enabled
      ? t('accounts.proxy.disabledSuffix')
      : proxy.lastTest?.success
        ? ''
        : proxy.lastTest
          ? t('accounts.proxy.testFailed')
          : t('accounts.proxy.notTested')}`,
    value: proxy.id,
    description: proxy.name.trim()
      ? proxy.endpoint
      : undefined,
    disabled: !proxy.enabled,
  })),
])
const selection = computed({
  get: () => mode.value === 'proxy'
    ? proxyId.value
    : mode.value === 'preserve' && props.accountId
      ? currentSelection.value
      : mode.value,
  set: (value: string) => {
    // 显示已绑定代理，未改选择时仍省略写入，保留禁用代理及仅地址配置
    const nextMode = props.preserve && props.accountId && value === currentSelection.value
      ? 'preserve'
      : value === 'preserve' || value === 'direct'
        ? value
        : 'proxy'
    mode.value = nextMode
    proxyId.value = nextMode === 'proxy'
      ? value
      : ''
  },
})
const selectedProxy = computed(() => proxies.value.find(proxy => proxy.id === selection.value))
const testTarget = computed(() => {
  if (props.disabled || loading.value)
    return null
  // 禁用代理的账号实际走直连，不把该代理自身的测试结果当成账号出口
  if (selection.value === 'direct' || selectedProxy.value?.enabled === false)
    return 'direct' as const
  return selectedProxy.value ?? null
})
const { result, testing, error, test } = useAccountProxyTest({
  target: () => testTarget.value,
  accountId: () => props.accountId,
  onTested: (record) => {
    proxies.value = proxies.value.map(proxy => proxy.id === record.id
      ? record
      : proxy)
  },
})
const testDescription = computed(() => selectedProxy.value?.enabled === false
  ? t('accounts.proxy.disabledUsesDirect')
  : selection.value !== 'direct' && !selectedProxy.value
    ? t('accounts.proxy.selectToTest')
    : t('accounts.proxy.testHelp'))
</script>

<template>
  <ZFormItem :prop="formProp" :show-message="false" :label="t('accounts.proxy.outboundTunnel')">
    <div class="grid w-full min-w-0 gap-2">
      <div class="flex min-w-0 gap-2">
        <ZSelect v-model="selection" class="min-w-0 flex-1" :options="options" :disabled="disabled || loading" :aria-label="t('accounts.proxy.outboundTunnel')" />
        <ZButton class="shrink-0" :loading="testing" :disabled="!testTarget" @click="test">
          <template #icon>
            <Wifi class="size-4" />
          </template>
          {{ t('accounts.proxy.testExit') }}
        </ZButton>
      </div>
      <p class="m-0 text-z-xs text-z-text-secondary">
        {{ testDescription }}
      </p>
      <div v-if="result" class="grid min-w-0 gap-1 text-z-xs" role="status">
        <span :class="result.success ? 'text-z-success-text' : 'text-z-error-text'">
          {{ result.success ? t('accounts.proxy.connected', { latencyMs: result.latencyMs }) : result.message }}
        </span>
        <span v-if="result.exitIpv4" class="font-mono break-all">IPv4 · {{ result.exitIpv4 }}</span>
        <span v-if="result.exitIpv6" class="font-mono break-all">IPv6 · {{ result.exitIpv6 }}</span>
        <span v-if="!result.exitIpv4 && !result.exitIpv6 && result.exitIp" class="font-mono break-all">IP · {{ result.exitIp }}</span>
      </div>
      <p v-if="error" class="m-0 text-z-xs wrap-break-word text-z-error-text" role="alert">
        {{ error }}
      </p>
    </div>
  </ZFormItem>
</template>
