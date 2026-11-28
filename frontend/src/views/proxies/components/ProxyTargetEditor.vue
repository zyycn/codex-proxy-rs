<script setup lang="ts">
import type { ProxyTestTarget } from '@/api'
import { ZButton, ZIconButton, ZInput } from '@codex-proxy/ui'
import { Plus, Trash2 } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled: boolean }>()
const targets = defineModel<ProxyTestTarget[]>({ required: true })
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <div class="grid gap-3">
    <div v-for="(target, index) in targets" :key="index" class="flex items-center gap-2">
      <div class="grid min-w-0 flex-1 gap-2 sm:grid-cols-[1fr_2fr]">
        <ZInput v-model="target.name" maxlength="100" :disabled="disabled" :aria-label="t('proxies.targets.name')" :placeholder="t('proxies.targets.name')" />
        <ZInput v-model="target.url" maxlength="2048" :disabled="disabled" :aria-label="t('proxies.targets.address')" :placeholder="t('proxies.targets.address')" />
      </div>
      <ZIconButton :aria-label="t('proxies.targets.remove')" :disabled="disabled" @click="targets.splice(index, 1)">
        <Trash2 class="size-4" />
      </ZIconButton>
    </div>
    <ZButton class="justify-self-start" :disabled="disabled || targets.length >= 32" @click="targets.push({ name: '', url: '' })">
      <Plus class="size-4" />
      {{ t('proxies.targets.add') }}
    </ZButton>
  </div>
</template>
