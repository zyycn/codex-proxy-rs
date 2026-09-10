<script setup lang="ts">
import { Upload } from '@lucide/vue'
import { useFileDialog } from '@vueuse/core'
import { onScopeDispose, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

defineProps<{
  label: string
  placeholder: string
  uploadable: boolean
  disabled: boolean
}>()
const text = defineModel<string>({ required: true })
const fileError = ref('')
const { open: openFile, onChange } = useFileDialog({ accept: 'application/json,.json', multiple: false, reset: true })

let readVersion = 0
onScopeDispose(() => {
  readVersion += 1
})

onChange(async (files) => {
  const file = files?.[0]
  if (!file)
    return
  const version = ++readVersion
  fileError.value = ''
  try {
    const contents = await file.text()
    if (version !== readVersion)
      return
    text.value = contents
  }
  catch {
    if (version === readVersion)
      fileError.value = '文件读取失败，请重新选择'
  }
})

function updateText(value: string) {
  text.value = value
  readVersion += 1
  fileError.value = ''
}
</script>

<template>
  <BaseFormItem :label="label" required :error="fileError || undefined">
    <template v-if="uploadable" #extra>
      <BaseButton size="sm" :disabled="disabled" @click="openFile()">
        <template #icon>
          <Upload class="size-3.5" aria-hidden="true" />
        </template>
        上传文件
      </BaseButton>
    </template>
    <BaseTextarea
      :model-value="text"
      :aria-label="label"
      :rows="9"
      :placeholder="placeholder"
      :disabled="disabled"
      @update:model-value="updateText"
    />
  </BaseFormItem>
</template>
