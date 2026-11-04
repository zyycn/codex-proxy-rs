<script setup lang="ts">
import type { OutboundProxyTest } from '@/api'
import { ZFormItem, ZIconButton, ZSelect } from '@codex-proxy/ui'
import { Wifi } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useProxyCatalog } from '@/composables/useProxyCatalog'
import { useAccountProxyTest } from '../composables/useAccountProxyTest'
import AccountProxyTestBadge from './AccountProxyTestBadge.vue'

const props = withDefaults(defineProps<{ accountId?: string, currentProxyId?: string | null, endpoint?: string | null, disabled?: boolean, preserve?: boolean, formProp: string }>(), { preserve: true })

const { t } = useI18n({ useScope: 'global' })

const mode = defineModel<string>('mode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading, loadProxies } = useProxyCatalog()
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
// 目录刷新只禁用测试入口，不改变目标，避免清除版本冲突提示
const testTarget = computed(() => {
  if (props.disabled)
    return null
  // 禁用代理的账号实际走直连，不把该代理自身的测试结果当成账号出口
  if (selection.value === 'direct' || selectedProxy.value?.enabled === false)
    return 'direct' as const
  return selectedProxy.value ?? null
})
const { result, directResult, testing, test } = useAccountProxyTest({
  target: () => testTarget.value,
  accountId: () => props.accountId,
  onConflict: loadProxies,
  onTested: (record) => {
    proxies.value = proxies.value.map(proxy => proxy.id === record.id
      ? record
      : proxy)
  },
})
// 代理复用目录中的最近测试，直连结果只保留在当前账号的编辑过程
const optionTests = computed<Record<string, OutboundProxyTest | null>>(() => ({
  ...Object.fromEntries(proxies.value.map(proxy => [
    proxy.id,
    proxy.enabled ? proxy.lastTest : null,
  ])),
  direct: directResult.value,
}))
const selectedTest = computed(() => result.value ?? (testTarget.value === 'direct'
  ? directResult.value
  : optionTests.value[selection.value]))
const testHint = computed(() => selectedProxy.value?.enabled === false
  ? t('accounts.proxy.disabledUsesDirect')
  : selection.value !== 'direct' && !selectedProxy.value
    ? t('accounts.proxy.selectToTest')
    : null)
</script>

<template>
  <ZFormItem :prop="formProp" :show-message="false" :label="t('accounts.proxy.outboundTunnel')">
    <div class="grid w-full min-w-0 gap-1.5">
      <div class="flex min-w-0 gap-2">
        <div class="relative min-w-0 flex-1">
          <!-- 为框内测试结果预留空间，避免遮挡代理文本和下拉箭头 -->
          <ZSelect v-model="selection" class="w-full" :class="{ '[&_[role=combobox]]:pr-28': selectedTest?.success }" :options="options" :disabled="disabled || loading" :aria-label="t('accounts.proxy.outboundTunnel')">
            <template #option="{ option, selected }">
              <span class="min-w-0 truncate" :class="option.description ? 'max-w-1/2 shrink-0' : 'flex-1'">{{ option.label }}</span>
              <span v-if="option.description" class="min-w-0 flex-1 truncate font-normal" :class="option.disabled ? 'text-z-text-disabled' : 'text-z-text-tertiary'" :title="option.description">
                {{ option.description }}
              </span>
              <AccountProxyTestBadge class="ml-auto" :result="optionTests[option.value]" in-option />
              <span v-if="!selected" class="size-[15px] shrink-0" aria-hidden="true" />
            </template>
          </ZSelect>
          <div v-if="selectedTest?.success" class="absolute inset-y-0 right-9 flex items-center">
            <AccountProxyTestBadge :result="selectedTest" />
          </div>
        </div>
        <ZIconButton class="shrink-0" variant="solid" :disabled="testing || loading || !testTarget" :aria-label="t('accounts.proxy.testExit')" @click="test">
          <Wifi class="size-4" />
        </ZIconButton>
      </div>
      <p v-if="testHint" class="m-0 text-z-xs text-z-text-secondary">
        {{ testHint }}
      </p>
    </div>
  </ZFormItem>
</template>
