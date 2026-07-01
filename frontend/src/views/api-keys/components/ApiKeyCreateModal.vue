<script setup lang="ts">
import { Copy } from '@lucide/vue'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'

defineProps<{
  createdKey: string
  saving: boolean
}>()

const open = defineModel<boolean>({ default: false })
const createdOpen = defineModel<boolean>('createdOpen', { default: false })
const form = defineModel<any>('form', { required: true })

const emit = defineEmits<{
  create: []
  copy: [text: string]
}>()

function formField(key: string) {
  return computed({
    get: () => form.value[key] ?? '',
    set: (value: string) => {
      form.value = { ...form.value, [key]: value }
    },
  })
}

const name = formField('name')
const label = formField('label')
</script>

<template>
  <BaseModal
    v-model="open"
    title="创建 API Key"
    description="创建后只显示一次，请立即保存。"
    variant="info"
    width="540px"
    :close-disabled="saving"
  >
    <BaseForm>
      <BaseFormItem label="名称" required>
        <BaseInput v-model="name" placeholder="例如：生产环境、测试账号..." />
      </BaseFormItem>

      <BaseFormItem label="标签（可选）">
        <BaseInput v-model="label" placeholder="备注信息..." />
      </BaseFormItem>
    </BaseForm>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">取消</BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!name.trim()"
        @click="emit('create')"
      >
        创建
      </BaseButton>
    </template>
  </BaseModal>

  <BaseModal
    v-model="createdOpen"
    title="API Key 已创建"
    description="密钥只会显示一次，关闭弹窗后无法再次查看完整内容。"
    variant="success"
    width="540px"
  >
    <div class="flex flex-col gap-4">
      <div
        class="rounded-(--cp-input-radius-base) border border-(--cp-warning-border) bg-(--cp-warning-bg) px-4 py-3"
      >
        <p class="m-0 text-[13px] font-semibold text-(--cp-warning-text)">
          请妥善保存此密钥，它只会显示一次。
        </p>
      </div>

      <div>
        <label class="block text-[13px] font-medium text-(--cp-text-secondary) mb-2">
          API Key
        </label>
        <div class="flex items-center gap-2">
          <code
            class="flex-1 px-3 py-2.5 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) text-[13px] font-mono text-(--cp-text-primary) break-all"
          >
            {{ createdKey }}
          </code>
          <BaseButton icon-only size="md" title="复制" @click="emit('copy', createdKey)">
            <Copy class="size-4" />
          </BaseButton>
        </div>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="createdOpen = false">我已保存</BaseButton>
    </template>
  </BaseModal>
</template>
