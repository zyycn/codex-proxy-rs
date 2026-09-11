<script setup lang="ts">
import { ExternalLink, RefreshCw } from '@lucide/vue'
import { computed } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { useProxyCatalog } from '@/composables/useProxyCatalog'

const props = withDefaults(defineProps<{ accountId?: string, endpoint?: string | null, disabled?: boolean, preserve?: boolean, error?: string }>(), { preserve: true })
const mode = defineModel<string>('mode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const { proxies, loading, loadProxies } = useProxyCatalog()
const current = computed(() => proxies.value.find(proxy => proxy.accounts.some(account => account.id === props.accountId)))
const options = computed(() => [
  ...(props.preserve ? [{ label: current.value ? `保持当前：${current.value.name}` : '保持当前', value: 'preserve' }] : []),
  { label: '直连', value: 'direct' },
  ...proxies.value.map(proxy => ({
    label: `${proxy.name}${proxy.lastTest?.success ? '' : proxy.lastTest ? '（测试失败）' : '（未测试）'}`,
    value: proxy.id,
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
  <div class="grid min-w-0 grid-cols-1 gap-3">
    <BaseFormItem label="出站隧道" :error="error">
      <div class="flex min-w-0 items-center gap-2">
        <BaseSelect v-model="selection" class="min-w-0 flex-1" :options="options" :disabled="disabled || loading" aria-label="出站隧道" />
        <BaseIconButton label="刷新可选代理" :disabled="disabled || loading" @click="loadProxies()">
          <RefreshCw class="size-4" />
        </BaseIconButton>
        <a href="/proxies" target="_blank" rel="noopener noreferrer" class="inline-flex size-8 shrink-0 items-center justify-center text-cp-link" aria-label="打开代理管理" title="打开代理管理"><ExternalLink class="size-4" /></a>
      </div>
    </BaseFormItem>
    <p v-if="endpoint && mode === 'preserve'" class="m-0 font-mono text-xs break-all text-cp-text-tertiary">
      {{ endpoint }}
    </p>
    <p v-if="mode === 'proxy'" class="m-0 break-all font-mono text-xs text-cp-text-secondary">
      {{ proxies.find(proxy => proxy.id === proxyId)?.endpoint }}
    </p>
  </div>
</template>
