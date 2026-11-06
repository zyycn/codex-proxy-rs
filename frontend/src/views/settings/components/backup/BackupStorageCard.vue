<script setup lang="ts">
import { ZButton, ZCard, ZCheckbox, ZForm, ZFormItem, ZInput } from '@codex-proxy/ui'

import { CircleAlert, CircleCheck, DatabaseZap, Save } from '@lucide/vue'

import { I18nT, useI18n } from 'vue-i18n'

defineProps<{
  disabled: boolean
  saving: boolean
  testing: boolean
  verified: boolean
}>()

const emit = defineEmits<{
  save: []
  test: []
  openR2Guide: []
}>()

const { t } = useI18n({ useScope: 'global' })

interface StorageForm {
  endpoint: string
  region: string
  bucket: string
  accessKeyId: string
  secretAccessKey: string
  prefix: string
  forcePathStyle: boolean
}

const storage = defineModel<StorageForm>('storage', { required: true })

function updateStorage<Key extends keyof StorageForm>(key: Key, value: StorageForm[Key]) {
  storage.value = { ...storage.value, [key]: value }
}
</script>

<template>
  <ZCard :title="t('settings.backup.storage.s3StorageConfiguration')">
    <template #description>
      <I18nT keypath="settings.backup.storage.description" tag="span" scope="global" class="text-z-text-secondary">
        <template #service>
          <ZButton
            variant="link"
            class="h-auto! rounded-none! p-0! text-z-link! underline underline-offset-2 hover:text-z-link-hover! [font:inherit]!"
            @click="emit('openR2Guide')"
          >
            Cloudflare R2
          </ZButton>
        </template>
      </I18nT>
    </template>

    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <ZButton
          :loading="testing"
          :disabled="disabled"
          :title="verified ? t('settings.backup.storage.connectionTestPassed') : t('settings.backup.storage.connectionTestHasNotPassed')"
          @click="emit('test')"
        >
          <template #icon>
            <CircleCheck v-if="verified" class="size-4 text-z-success-text" />
            <CircleAlert v-else class="size-4 text-z-warning-text" />
          </template>
          {{ testing ? t('settings.backup.storage.testing') : t('settings.backup.storage.test') }}
        </ZButton>
        <ZButton type="primary" :loading="saving" :disabled="disabled" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? t('settings.backup.schedule.saving') : t('common.actions.save') }}
        </ZButton>
      </div>
    </template>

    <div class="@container">
      <ZForm class="max-w-6xl @min-[640px]:grid-cols-2">
        <ZFormItem :label="t('settings.backup.storage.endpointAddress')" :description="t('settings.backup.storage.endpointHelp')">
          <ZInput
            :model-value="storage.endpoint" :disabled="disabled"
            :aria-label="t('settings.backup.storage.endpointAddress')"
            placeholder="https://<account_id>.r2.cloudflarestorage.com"
            @update:model-value="updateStorage('endpoint', $event)"
          >
            <template #prefix>
              <DatabaseZap class="size-4" />
            </template>
          </ZInput>
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.storage.region')" :description="t('settings.backup.storage.regionHelp')">
          <ZInput :model-value="storage.region" :disabled="disabled" :aria-label="t('settings.backup.storage.region')" @update:model-value="updateStorage('region', $event)" />
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.storage.bucket')" :description="t('settings.backup.storage.privateBucketName')">
          <ZInput :model-value="storage.bucket" :disabled="disabled" :aria-label="t('settings.backup.storage.bucket')" @update:model-value="updateStorage('bucket', $event)" />
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.storage.objectKeyPrefix')" :description="t('settings.backup.storage.prefixHelp')">
          <ZInput :model-value="storage.prefix" :disabled="disabled" :aria-label="t('settings.backup.storage.objectKeyPrefix')" @update:model-value="updateStorage('prefix', $event)" />
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.storage.accessKeyId')" :description="t('settings.backup.storage.credentialsLabel')">
          <ZInput
            :model-value="storage.accessKeyId" :disabled="disabled"
            :aria-label="t('settings.backup.storage.accessKeyId')"
            show-password
            autocomplete="off"
            @update:model-value="updateStorage('accessKeyId', $event)"
          />
        </ZFormItem>

        <ZFormItem :label="t('settings.backup.storage.secretAccessKey')" :description="t('settings.backup.storage.objectStorageSpecificSecret')">
          <ZInput
            :model-value="storage.secretAccessKey" :disabled="disabled"
            :aria-label="t('settings.backup.storage.secretAccessKey')"
            show-password
            autocomplete="new-password"
            @update:model-value="updateStorage('secretAccessKey', $event)"
          />
        </ZFormItem>

        <div class="col-span-2 flex items-center gap-4 @max-[640px]:col-span-1">
          <ZCheckbox :model-value="storage.forcePathStyle" :disabled="disabled" :label="t('settings.backup.storage.forcePathStyle')" @update:model-value="updateStorage('forcePathStyle', $event)" />
        </div>
      </ZForm>
    </div>
  </ZCard>
</template>
