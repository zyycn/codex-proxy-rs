<script setup lang="ts">
import type { ClientProfileSelection, XaiClientProfileSelection } from '@/api/modules/client-profiles'
import { Openai, Xai } from '@boxicons/vue'
import { shallowRef } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import ClientProfileEditor from '@/components/client-profile/ClientProfileEditor.vue'
import XaiClientProfileEditor from '@/components/client-profile/XaiClientProfileEditor.vue'
import { PROVIDER_DISPLAY_NAMES } from '@/utils/providers'

withDefaults(defineProps<{ active?: boolean, disabled?: boolean }>(), {
  active: true,
  disabled: false,
})

const openai = defineModel<ClientProfileSelection | null>('openai', { required: true })
const xai = defineModel<XaiClientProfileSelection | null>('xai', { required: true })
const provider = shallowRef('openai')
const providerOptions = [
  { label: PROVIDER_DISPLAY_NAMES.openai, value: 'openai', icon: Openai },
  { label: PROVIDER_DISPLAY_NAMES.xai, value: 'xai', icon: Xai },
]
</script>

<template>
  <BaseCard>
    <template #header>
      <div class="flex max-w-6xl items-start justify-between gap-4">
        <div class="min-w-0 pt-0.5">
          <h2 class="m-0 text-xl leading-[1.15] font-heavy text-cp-text text-balance">
            客户端身份
          </h2>
          <p class="mt-1.75 mb-0 text-cp leading-[1.3] font-emphasis text-cp-text-secondary text-pretty">
            配置网关向上游声明的客户端类型、版本与请求头
          </p>
        </div>
        <BaseSegmented
          v-model="provider"
          label="客户端身份平台"
          class="w-20 shrink-0"
          display="icon"
          :options="providerOptions"
          :disabled="disabled"
        />
      </div>
    </template>
    <ClientProfileEditor
      v-if="openai"
      v-show="provider === 'openai'"
      v-model="openai"
      :active="active"
      :disabled="disabled"
      class="max-w-6xl"
    />
    <XaiClientProfileEditor
      v-if="xai"
      v-show="provider === 'xai'"
      v-model="xai"
      :active="active"
      :disabled="disabled"
      class="max-w-6xl"
    />
  </BaseCard>
</template>
