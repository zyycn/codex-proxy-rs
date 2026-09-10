<script setup lang="ts">
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
  }>(),
  { preserve: true },
)
const mode = defineModel<string>('mode', { required: true })
const url = defineModel<string>('url', { required: true })
const options = computed(() => [
  ...(props.preserve ? [{ label: '保持当前', value: 'preserve' }] : []),
  { label: '直连', value: 'direct' },
  { label: '指定代理', value: 'proxy' },
])
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
