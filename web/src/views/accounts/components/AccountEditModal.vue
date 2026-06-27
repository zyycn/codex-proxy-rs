<script setup lang="ts">
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

defineProps<{
  account: any
  statusOptions: any[]
  saving: boolean
}>()

const open = defineModel<boolean>({ default: false })
const form = defineModel<any>('form', { required: true })
const status = defineModel<string>('status', { required: true })

const emit = defineEmits<{
  save: []
}>()

function formField(key: string) {
  return computed({
    get: () => form.value[key] ?? '',
    set: (value: string) => {
      form.value = { ...form.value, [key]: value }
    },
  })
}

const label = formField('label')
const email = formField('email')
const accountId = formField('accountId')
const userId = formField('userId')
const planType = formField('planType')
</script>

<template>
  <BaseModal
    v-model="open"
    title="编辑账号"
    description="更新账号元信息、套餐和调度状态。"
    variant="info"
    width="680px"
    :close-disabled="saving"
  >
    <div class="grid gap-4 sm:grid-cols-2">
      <div class="sm:col-span-2">
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
          内部 ID
        </label>
        <div
          class="flex h-10 items-center truncate rounded-lg bg-(--cp-bg-subtle) px-3 font-mono text-[12px] font-[650] text-(--cp-text-muted)"
        >
          {{ account?.id || '-' }}
        </div>
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
          备注标签
        </label>
        <BaseInput v-model="label" placeholder="例如：主账号 / 备用账号" />
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">邮箱</label>
        <BaseInput v-model="email" placeholder="account@example.com" />
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
          ChatGPT 账号 ID
        </label>
        <BaseInput v-model="accountId" placeholder="chatgpt account id" />
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">
          用户 ID
        </label>
        <BaseInput v-model="userId" placeholder="user id" />
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">套餐</label>
        <BaseInput v-model="planType" placeholder="free / plus / pro / team" />
      </div>

      <div>
        <label class="mb-2 block text-[13px] font-medium text-(--cp-text-secondary)">状态</label>
        <BaseSelect v-model="status" :options="statusOptions" />
      </div>
    </div>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">取消</BaseButton>
      <BaseButton variant="primary" :loading="saving" @click="emit('save')">保存</BaseButton>
    </template>
  </BaseModal>
</template>
