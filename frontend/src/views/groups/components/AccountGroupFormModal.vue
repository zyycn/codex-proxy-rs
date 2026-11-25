<script setup lang="ts">
import type { FormRules } from '@codex-proxy/ui'
import type { AccountGroupFormValue } from '../composables/useAccountGroups'

import type { AccountGroup } from '@/api'
import type { BuiltinProvider } from '@/providers/catalog'
import { normalizeRgbaHexColor, ZButton, ZColorPicker, ZDialog, ZEmpty, ZForm, ZFormItem, ZInput, ZSegmented, ZTextarea } from '@codex-proxy/ui'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'
import AccountModelAccessField from '@/components/account-model-access-field/index.vue'
import { accountModelAccessError } from '@/components/account-model-access-field/validation'
import HelpPopover from '@/components/HelpPopover.vue'
import ProviderSettingsSection from '@/components/ProviderSettingsSection.vue'
import { ACCOUNT_GROUP_COLOR_PRESETS } from '../constants'

const props = defineProps<{
  group: AccountGroup | null
  saving: boolean
}>()

const emit = defineEmits<{
  save: []
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const form = defineModel<AccountGroupFormValue>('form', { required: true })
const provider = shallowRef<BuiltinProvider>('openai')
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const rules = computed<FormRules<AccountGroupFormValue>>(() => ({
  name: {
    required: true,
    whitespace: true,
    trigger: ['blur', 'change'],
    message: t('groups.actions.enterGroupName'),
  },
  modelAccess: {
    trigger: [],
    validator: () => {
      const error = accountModelAccessError(form.value.modelAccess)
      return error
        ? new Error(error)
        : true
    },
  },
  color: {
    validator: (_rule, value) => !!normalizeRgbaHexColor(value),
    trigger: 'change',
    message: t('groups.actions.selectValidGroupingColor'),
  },
}))
watch(open, (visible) => {
  formRef.value?.clearValidate()
  if (visible)
    provider.value = 'openai'
})

async function requestSave() {
  if (props.saving || !await formRef.value?.validate().catch(() => false))
    return
  emit('save')
}
const toggleOptions = computed(() => [
  {
    value: 'enabled',
    label: t('common.actions.enable'),
  },
  {
    value: 'disabled',
    label: t('common.actions.disable'),
  },
])
const title = computed(() => props.group
  ? t('groups.actions.editGroup')
  : t('groups.filters.createGroup'))
const description = computed(() => props.group
  ? t('groups.editor.editDescription')
  : t('groups.editor.createDescription'))
</script>

<template>
  <ZDialog
    v-model="open"
    :title="title"
    :description="description"
    width="42rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
  >
    <ZForm ref="formElement" :model="form" :rules="rules" class="sm:grid-cols-[minmax(0,1fr)_fit-content(10rem)] sm:gap-x-6 md:grid-cols-[minmax(0,1fr)_fit-content(11rem)]" scroll-to-error focus-on-error novalidate @submit="requestSave">
      <ZFormItem prop="name" :show-message="false" :label="t('groups.editor.groupName')" required>
        <ZInput
          v-model="form.name"
          :aria-label="t('groups.editor.groupName')"
          :placeholder="t('groups.editor.forExampleProductionAccount')"
          :disabled="saving"
        />
      </ZFormItem>
      <ZFormItem prop="color" :show-message="Boolean(form.color.trim())" :label="t('groups.editor.groupColor')" required>
        <ZColorPicker
          v-model="form.color"
          class="sm:pl-0"
          :aria-label="t('groups.editor.selectGroupColor')"
          :presets="ACCOUNT_GROUP_COLOR_PRESETS"
          :disabled="saving"
        />
      </ZFormItem>
      <ZFormItem :label="t('groups.editor.imageGeneration')" class="col-span-full">
        <ZSegmented
          :model-value="form.allowImageGeneration ? 'enabled' : 'disabled'"
          :aria-label="t('groups.editor.imageGeneration')"
          :options="toggleOptions"
          :disabled="saving"
          @update:model-value="form.allowImageGeneration = $event === 'enabled'"
        />
      </ZFormItem>
      <AccountModelAccessField
        class="col-span-full" :group-id="group?.id ?? null" :model-value="form.modelAccess" form-prop="modelAccess" :disabled="saving"
        @update:model-value="value => { if (value) form.modelAccess = value }"
      />
      <ZFormItem :label="t('groups.editor.description')" class="col-span-full">
        <ZTextarea
          v-model="form.description"
          :aria-label="t('groups.editor.groupDescription')"
          :rows="2"
          :placeholder="t('groups.editor.describePurposeOfThisGroup')"
          :disabled="saving"
        />
      </ZFormItem>
      <ProviderSettingsSection v-model="provider" class="col-span-full" :disabled="saving">
        <div v-if="provider === 'openai'" class="flex flex-wrap items-start gap-x-6 gap-y-4.5">
          <ZFormItem :label="t('groups.editor.fastMode')">
            <template #label-extra>
              <HelpPopover :label="t('groups.editor.fastMode')" :content="t('groups.editor.fastModeHelp')" />
            </template>
            <ZSegmented
              v-model="form.fastMode"
              :aria-label="t('groups.editor.fastMode')"
              :options="[
                { label: t('common.values.default'), value: 'default' },
                { label: t('common.actions.disable'), value: 'disabled' },
                { label: t('groups.editor.forceFast'), value: 'enabled' },
                { label: t('groups.editor.forceUltrafast'), value: 'ultrafast' },
              ]"
              :disabled="saving"
            />
          </ZFormItem>
          <ZFormItem :label="t('groups.editor.subagents')">
            <template #label-extra>
              <HelpPopover :label="t('groups.editor.subagents')" :content="t('groups.editor.subagentsHelp')" />
            </template>
            <ZSegmented
              :model-value="form.allowSubagents ? 'enabled' : 'disabled'"
              :aria-label="t('groups.editor.subagents')"
              :options="toggleOptions"
              :disabled="saving"
              @update:model-value="form.allowSubagents = $event === 'enabled'"
            />
          </ZFormItem>
        </div>
        <ZEmpty v-else :title="t('groups.editor.noProviderSettings')" size="small" surface="none" />
      </ProviderSettingsSection>
    </ZForm>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        @click="requestSave"
      >
        {{ t('groups.editor.saveGroup') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
