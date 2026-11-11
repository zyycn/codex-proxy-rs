<script setup lang="ts">
import type { ProviderRequestProfiles } from '@/api/modules/settings/profiles'

import { ZCard, ZFormItem, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

withDefaults(defineProps<{ active?: boolean, disabled?: boolean }>(), {
  active: true,
  disabled: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ProviderRequestProfiles>({ required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading } = useProxyCatalog()
const proxyOptions = computed(() => [
  {
    label: t('settings.clientProfile.directConnection'),
    value: '',
  },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name}${proxy.enabled
      ? ''
      : t('settings.clientProfile.disabledProxy')}`,
    value: proxy.id,
    description: proxy.endpoint,
    disabled: !proxy.enabled,
  })),
  // 目录不可用时保留当前选择，避免把加载失败变成直连设置
  ...(proxyId.value && !proxies.value.some(proxy => proxy.id === proxyId.value)
    ? [
        {
          label: t('settings.clientProfile.proxyUnavailable'),
          value: proxyId.value,
          disabled: true,
        },
      ]
    : []),
])
</script>

<template>
  <ZCard>
    <ProviderRequestProfilesEditor
      v-model="model"
      :active="active"
      :disabled="disabled"
      class="max-w-6xl"
    >
      <template #heading>
        <div class="flex min-w-0 flex-1 flex-wrap items-start justify-between gap-3">
          <div class="min-w-0 pt-0.5">
            <h2 class="m-0 text-xl leading-[1.15] font-heavy text-z-text text-balance">
              {{ t('settings.clientProfile.clientIdentity') }}
            </h2>
            <p class="mt-1.75 mb-0 text-z leading-[1.3] font-emphasis text-z-text-secondary text-pretty">
              {{ t('settings.clientProfile.description') }}
            </p>
          </div>
          <ZFormItem :label="t('settings.clientProfile.releaseProxy')" class="mb-0 w-full sm:w-64">
            <ZSelect
              v-model="proxyId"
              :options="proxyOptions"
              :disabled="disabled || loading"
              :aria-label="t('settings.clientProfile.releaseProxy')"
              class="w-full"
            />
          </ZFormItem>
        </div>
      </template>
    </ProviderRequestProfilesEditor>
  </ZCard>
</template>
