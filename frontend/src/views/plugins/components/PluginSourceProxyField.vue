<script setup lang="ts">
import { ZFormItem, ZSelect } from '@codex-proxy/ui'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

defineProps<{ disabled?: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const proxyId = defineModel<string>({ required: true })
const { proxies, loading } = useProxyCatalog()
const options = computed(() => [
  {
    label: t('plugins.sourceProxy.noProxyDirectConnection'),
    value: '',
  },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name}${proxy.enabled
      ? ''
      : t('plugins.sourceProxy.disabledSuffix')}`,
    value: proxy.id,
    description: proxy.endpoint,
    disabled: !proxy.enabled,
  })),
  // 目录加载失败或代理已被删除时保留已选 ID，不能静默改为直连。
  ...(proxyId.value && !proxies.value.some(proxy => proxy.id === proxyId.value)
    ? [
        {
          label: t('plugins.sourceProxy.catalogUnavailable'),
          value: proxyId.value,
          description: proxyId.value,
          disabled: true,
        },
      ]
    : []),
])
</script>

<template>
  <ZFormItem :label="t('plugins.sourceProxy.label')">
    <ZSelect
      v-model="proxyId"
      :options="options"
      :disabled="disabled || loading"
      class="w-full"
      :aria-label="t('plugins.sourceProxy.label')"
    />
  </ZFormItem>
</template>
