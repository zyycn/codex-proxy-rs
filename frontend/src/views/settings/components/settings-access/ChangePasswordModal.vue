<script setup lang="ts">
import type { FormRules } from '@codex-proxy/ui'
import { ZButton, ZDialog, ZForm, ZFormItem, ZInput } from '@codex-proxy/ui'
import { computed, reactive, shallowRef, useId, useTemplateRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { changeAdminPassword } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notification as ZNotification } from '@/plugins/notification'
import { useAuthStore } from '@/stores/modules/auth'

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const formId = useId()
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const router = useRouter()
const auth = useAuthStore()
const { loading, run } = useAsyncAction()
const submitted = shallowRef(false)
const form = reactive({ currentPassword: '', newPassword: '', confirmation: '' })
const rules = computed<FormRules<typeof form>>(() => ({
  currentPassword: {
    required: true,
    message: t('settings.password.enterCurrentPassword'),
  },
  newPassword: [
    {
      required: true,
      message: t('settings.password.validation.minLength'),
    },
    {
      validator: (_rule, value: string) => Array.from(value.trim()).length >= 12,
      message: t('settings.password.validation.minLength'),
    },
    {
      validator: (_rule, value: string) => new TextEncoder().encode(value).length <= 1024,
      message: t('settings.password.newPasswordCannotExceedBytes'),
    },
    {
      validator: (_rule, value: string) => value !== form.currentPassword,
      message: t('settings.password.validation.unchanged'),
    },
  ],
  confirmation: [
    {
      required: true,
      message: t('settings.password.enterNewPasswordAgain'),
    },
    {
      validator: (_rule, value: string) => value === form.newPassword,
      message: t('settings.password.validation.mismatch'),
    },
  ],
}))

function revalidateDependentField(prop: 'newPassword' | 'confirmation') {
  if (submitted.value)
    void formRef.value?.getField(prop)?.validate('change').catch(() => false)
}

function clearForm() {
  Object.assign(form, { currentPassword: '', newPassword: '', confirmation: '' })
  submitted.value = false
  formRef.value?.clearValidate()
}

watch(open, (value) => {
  if (value)
    clearForm()
})

async function submit() {
  submitted.value = true
  if (loading.value || !await formRef.value?.validate().catch(() => false))
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
  <ZDialog v-model="open" destroy-on-close :title="t('settings.password.changeAdministratorPassword')" :description="t('settings.password.changeDescription')" width="28rem" :show-close="!loading" :close-on-click-modal="!loading" :close-on-press-escape="!loading" @closed="clearForm">
    <ZForm :id="formId" ref="formElement" class="gap-5" :model="form" :rules="rules" :validate-on-rule-change="false" focus-on-error novalidate @submit="submit">
      <ZFormItem prop="currentPassword" :label="t('settings.password.currentPassword')">
        <ZInput v-model="form.currentPassword" :validate-event="submitted" type="password" autocomplete="current-password" :placeholder="t('settings.password.enterCurrentPassword')" :disabled="loading" maxlength="4096" @input="revalidateDependentField('newPassword')" />
      </ZFormItem>
      <ZFormItem prop="newPassword" :label="t('settings.password.newPassword')" :description="t('settings.password.passwordHelp')">
        <ZInput v-model="form.newPassword" :validate-event="submitted" type="password" autocomplete="new-password" :placeholder="t('settings.password.setNewPassword')" :disabled="loading" maxlength="1024" @input="revalidateDependentField('confirmation')" />
      </ZFormItem>
      <ZFormItem prop="confirmation" :label="t('settings.password.confirmNewPassword')">
        <ZInput v-model="form.confirmation" :validate-event="submitted" type="password" autocomplete="new-password" :placeholder="t('settings.password.enterNewPasswordAgain')" :disabled="loading" maxlength="1024" />
      </ZFormItem>
    </ZForm>
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
