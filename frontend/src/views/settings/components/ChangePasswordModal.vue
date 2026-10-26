<script setup lang="ts">
import { ZFormItem as FormItem, ZButton, ZDialog, ZInput } from '@codex-proxy/ui'
import { computed, reactive, shallowRef, useId, watch } from 'vue'

import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { changeAdminPassword } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useAuthStore } from '@/stores/modules/auth'
import { notification as ZNotification } from '@/utils/notification'

const { t } = useI18n({ useScope: 'global' })

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
  if ([
    ...form.newPassword.trim(),
  ].length < 12) {
    return t('settings.password.validation.minLength')
  }
  if (new TextEncoder().encode(form.newPassword).length > 1024)
    return t('settings.password.newPasswordCannotExceedBytes')
  if (form.newPassword === form.currentPassword)
    return t('settings.password.validation.unchanged')
  return undefined
})
const confirmationError = computed(() => submitted.value && form.confirmation !== form.newPassword
  ? t('settings.password.validation.mismatch')
  : undefined)

function clearForm() {
  Object.assign(form, { currentPassword: '', newPassword: '', confirmation: '' })
  submitted.value = false
}

watch(open, (value) => {
  if (value)
    clearForm()
})

async function submit() {
  submitted.value = true
  if (!form.currentPassword || passwordError.value || confirmationError.value)
    return
  await run(async () => {
    await changeAdminPassword({ currentPassword: form.currentPassword, newPassword: form.newPassword })
    auth.invalidateSession()
    open.value = false
    ZNotification.success({
      message: t('settings.password.changed'),
    })
    await router.replace('/login')
  })
}
</script>

<template>
  <ZDialog v-model="open" :title="t('settings.password.changeAdministratorPassword')" :description="t('settings.password.changeDescription')" width="28rem" :show-close="!loading" :close-on-click-modal="!loading" :close-on-press-escape="!loading" @closed="clearForm">
    <form :id="formId" class="grid gap-5" @submit.prevent="submit">
      <FormItem :label="t('settings.password.currentPassword')" required>
        <ZInput v-model="form.currentPassword" type="password" autocomplete="current-password" :placeholder="t('settings.password.enterCurrentPassword')" :disabled="loading" maxlength="4096" />
      </FormItem>
      <FormItem :label="t('settings.password.newPassword')" :description="t('settings.password.passwordHelp')" :error="passwordError" required>
        <ZInput v-model="form.newPassword" type="password" autocomplete="new-password" :placeholder="t('settings.password.setNewPassword')" :disabled="loading" maxlength="1024" />
      </FormItem>
      <FormItem :label="t('settings.password.confirmNewPassword')" :error="confirmationError" required>
        <ZInput v-model="form.confirmation" type="password" autocomplete="new-password" :placeholder="t('settings.password.enterNewPasswordAgain')" :disabled="loading" maxlength="1024" />
      </FormItem>
    </form>
    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton native-type="submit" :form="formId" type="primary" :loading="loading">
        {{ t('settings.password.saveAndLogInAgain') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
