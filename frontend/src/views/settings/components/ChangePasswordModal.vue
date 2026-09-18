<script setup lang="ts">
import { computed, reactive, shallowRef, useId, watch } from 'vue'
import { useRouter } from 'vue-router'
import { changeAdminPassword } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import FormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useAuthStore } from '@/stores/modules/auth'

const open = defineModel<boolean>({ default: false })
const formId = useId()
const router = useRouter()
const auth = useAuthStore()
const { loading, run } = useAsyncAction()
const submitted = shallowRef(false)
const form = reactive({ currentPassword: '', newPassword: '', confirmation: '' })
const passwordError = computed(() => {
  if (!submitted.value)
    return undefined
  if ([...form.newPassword.trim()].length < 12)
    return '新密码至少需要 12 个字符'
  if (new TextEncoder().encode(form.newPassword).length > 1024)
    return '新密码不能超过 1024 字节'
  if (form.newPassword === form.currentPassword)
    return '新密码不能与当前密码相同'
  return undefined
})
const confirmationError = computed(() => submitted.value && form.confirmation !== form.newPassword
  ? '两次输入的新密码不一致'
  : undefined)

watch(open, () => {
  Object.assign(form, { currentPassword: '', newPassword: '', confirmation: '' })
  submitted.value = false
})

async function submit() {
  submitted.value = true
  if (!form.currentPassword || passwordError.value || confirmationError.value)
    return
  await run(async () => {
    await changeAdminPassword({ currentPassword: form.currentPassword, newPassword: form.newPassword })
    auth.invalidateSession()
    open.value = false
    toast.success('密码已修改，请使用新密码重新登录')
    await router.replace('/login')
  })
}
</script>

<template>
  <BaseModal v-model="open" title="修改管理员密码" description="验证当前密码后，设置新的登录密码" size="sm" :dismissible="!loading">
    <form :id="formId" class="grid gap-5" @submit.prevent="submit">
      <FormItem label="当前密码" required>
        <BaseInput v-model="form.currentPassword" type="password" autocomplete="current-password" placeholder="输入当前密码" :disabled="loading" maxlength="4096" />
      </FormItem>
      <FormItem label="新密码" description="至少 12 个字符，建议混合使用字母、数字和符号" :error="passwordError" required>
        <BaseInput v-model="form.newPassword" type="password" autocomplete="new-password" placeholder="设置新密码" :disabled="loading" maxlength="1024" />
      </FormItem>
      <FormItem label="确认新密码" :error="confirmationError" required>
        <BaseInput v-model="form.confirmation" type="password" autocomplete="new-password" placeholder="再次输入新密码" :disabled="loading" maxlength="1024" />
      </FormItem>
      <p class="m-0 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 text-cp-sm leading-relaxed text-cp-text-secondary">
        保存后，当前及其他设备上的管理员登录都会失效，需要重新登录。
      </p>
    </form>
    <template #footer>
      <BaseButton variant="secondary" :disabled="loading" @click="open = false">
        取消
      </BaseButton>
      <BaseButton type="submit" :form="formId" variant="primary" :loading="loading">
        保存并重新登录
      </BaseButton>
    </template>
  </BaseModal>
</template>
