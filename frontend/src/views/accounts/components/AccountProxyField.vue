<script setup lang="ts">
import { computed } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

const props = withDefaults(defineProps<{ endpoint?: string | null, disabled?: boolean, preserve?: boolean }>(), { preserve: true })
const mode = defineModel<string>('mode', { required: true })
const url = defineModel<string>('url', { required: true })
const options = computed(() => [
  ...(props.preserve ? [{ label: '保持当前', value: 'preserve' }] : []),
  { label: '直连', value: 'direct' },
  { label: '指定代理', value: 'proxy' },
])
</script>

<template>
  <div class="grid gap-3">
    <BaseFormItem label="出站隧道">
      <BaseSelect v-model="mode" :options="options" :disabled="disabled" aria-label="出站隧道" />
    </BaseFormItem>
    <p v-if="endpoint && mode === 'preserve'" class="m-0 font-mono text-xs break-all text-cp-text-tertiary">
      {{ endpoint }}
    </p>
    <BaseFormItem v-if="mode === 'proxy'" label="代理 URL" required>
      <BaseInput v-model="url" type="password" autocomplete="new-password" aria-label="代理 URL" :disabled="disabled" />
    </BaseFormItem>
  </div>
</template>
