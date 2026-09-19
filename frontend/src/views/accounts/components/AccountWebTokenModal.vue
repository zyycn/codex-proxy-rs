<script setup lang="ts">
import type { AccountRow } from '../constants'
import type { Account } from '@/api'
import { Info } from '@lucide/vue'
import { ref, watch } from 'vue'

import { updateAccountWebToken } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountPlanBadge from './AccountPlanBadge.vue'

const props = defineProps<{
  account: AccountRow | null
}>()

const emit = defineEmits<{
  updated: [account: Account]
}>()

const open = defineModel<boolean>({ default: false })
const webAccessToken = ref('')
const saving = ref(false)
const actionError = ref('')

watch(open, (isOpen) => {
  if (isOpen) {
    webAccessToken.value = ''
    actionError.value = ''
  }
})

async function handleSave(clear = false) {
  if (!props.account || saving.value)
    return

  const tokenToSubmit = clear ? null : webAccessToken.value.trim()
  if (!clear && !tokenToSubmit) {
    actionError.value = '请输入网页 Access Token'
    return
  }

  saving.value = true
  actionError.value = ''
  try {
    const res = await updateAccountWebToken({
      accountId: props.account.id,
      webAccessToken: tokenToSubmit || null,
    })
    toast.success(clear ? '已清除网页 Access Token' : '网页 Access Token 已保存')
    emit('updated', res.account)
    open.value = false
  }
  catch (error) {
    actionError.value = errorMessage(error)
  }
  finally {
    saving.value = false
  }
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="配置网页 Access Token"
    size="md"
    :dismissible="!saving"
  >
    <div v-if="account" class="grid gap-4">
      <div class="flex flex-wrap items-center justify-between gap-4 rounded-cp bg-cp-fill-quaternary px-4 py-3.5">
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="md"
        />
        <div class="flex shrink-0 items-center gap-2">
          <AccountPlanBadge
            :authentication-kind="account.authenticationKind"
            :plan-type="account.planType"
            :plan-type-display="account.planTypeDisplay"
            size="sm"
          />
        </div>
      </div>

      <div class="flex items-start gap-2.5 rounded-cp bg-cp-fill-quaternary p-3.5 text-cp-xs leading-normal text-cp-text-secondary">
        <Info class="mt-0.5 size-4 shrink-0 text-cp-primary" />
        <div class="space-y-1">
          <p class="m-0 font-medium text-cp-text">
            专用于额度充值卡（Rate Limit Reset Credits）
          </p>
          <p class="m-0 text-cp-text-tertiary">
            当账号以 Personal Access Token（<code class="rounded bg-cp-fill-tertiary px-1 py-0.5 font-mono text-[11px] text-cp-text">at-</code>）接入时，上游额度重置卡接口仅接受网页 Access Token。此处填入的 Token 仅用于额度重置，不会影响原有的模型 API 调用凭据。
          </p>
        </div>
      </div>

      <p
        v-if="actionError"
        class="m-0 rounded-cp bg-cp-error-container px-4 py-3 text-cp-xs leading-normal font-emphasis text-cp-error-on-container"
        role="alert"
      >
        {{ actionError }}
      </p>

      <BaseFormItem
        label="网页 Access Token"
        description="登录 chatgpt.com 后，在浏览器打开 https://chatgpt.com/api/auth/session 即可复制 accessToken 字段值（以 ey... 开头）。"
      >
        <BaseTextarea
          v-model="webAccessToken"
          :rows="4"
          placeholder="粘贴以 ey... 开头的网页 Access Token"
          :disabled="saving"
        />
      </BaseFormItem>
    </div>

    <template #footer>
      <div class="flex w-full items-center justify-between">
        <BaseButton
          variant="destructive"
          size="sm"
          :disabled="saving"
          @click="handleSave(true)"
        >
          清除 Token
        </BaseButton>
        <div class="flex items-center gap-2">
          <BaseButton
            variant="secondary"
            size="sm"
            :disabled="saving"
            @click="open = false"
          >
            取消
          </BaseButton>
          <BaseButton
            variant="primary"
            size="sm"
            :loading="saving"
            :disabled="saving"
            @click="handleSave(false)"
          >
            保存 Token
          </BaseButton>
        </div>
      </div>
    </template>
  </BaseModal>
</template>
