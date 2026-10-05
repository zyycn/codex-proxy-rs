<script setup lang="ts">
import { BaseButton, BaseCard, BaseCheckbox, BaseForm, BaseFormItem, BaseIconButton, BaseInput } from '@codex-proxy/ui'

import { CircleAlert, CircleCheck, DatabaseZap, Eye, EyeOff, Save } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

interface StorageForm {
  endpoint: string
  region: string
  bucket: string
  accessKeyId: string
  secretAccessKey: string
  prefix: string
  forcePathStyle: boolean
}

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

const storage = defineModel<StorageForm>('storage', { required: true })

// 凭据字段默认掩码显示，点击眼睛切换明文。
const accessKeyVisible = shallowRef(false)
const secretVisible = shallowRef(false)
const accessKeyInputType = computed(() => (accessKeyVisible.value ? 'text' : 'password'))
const secretInputType = computed(() => (secretVisible.value ? 'text' : 'password'))

function toggleAccessKeyVisible(): void {
  accessKeyVisible.value = !accessKeyVisible.value
}

function toggleSecretVisible(): void {
  secretVisible.value = !secretVisible.value
}

function updateStorage<Key extends keyof StorageForm>(key: Key, value: StorageForm[Key]) {
  storage.value = { ...storage.value, [key]: value }
}
</script>

<template>
  <BaseCard title="S3 存储配置">
    <template #description>
      <span class="text-cp-text-secondary">
        配置 S3 兼容存储（支持
        <button
          type="button"
          class="cursor-pointer border-0 bg-transparent p-0 text-cp-link underline underline-offset-2 hover:text-cp-link-hover"
          @click="emit('openR2Guide')"
        >
          Cloudflare R2
        </button>
        ）
      </span>
    </template>

    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <BaseButton
          variant="secondary"
          :loading="testing"
          :disabled="disabled"
          :title="verified ? '已通过连接测试' : '尚未通过连接测试'"
          @click="emit('test')"
        >
          <template #icon>
            <CircleCheck v-if="verified" class="size-4 text-cp-success-text" />
            <CircleAlert v-else class="size-4 text-cp-warning-text" />
          </template>
          {{ testing ? '测试中...' : '测试连接' }}
        </BaseButton>
        <BaseButton variant="primary" :loading="saving" :disabled="disabled" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存' }}
        </BaseButton>
      </div>
    </template>

    <div class="@container">
      <BaseForm class="max-w-6xl @min-[640px]:grid-cols-2">
        <BaseFormItem label="端点地址" description="S3 兼容服务的 HTTPS 地址">
          <BaseInput
            :model-value="storage.endpoint" :disabled="disabled"
            aria-label="端点地址"
            placeholder="https://<account_id>.r2.cloudflarestorage.com"
            @update:model-value="updateStorage('endpoint', $event)"
          >
            <template #prefix>
              <DatabaseZap class="size-4" />
            </template>
          </BaseInput>
        </BaseFormItem>

        <BaseFormItem label="区域" description="R2 使用固定值 auto，其它服务按提供方填写">
          <BaseInput :model-value="storage.region" :disabled="disabled" aria-label="区域" @update:model-value="updateStorage('region', $event)" />
        </BaseFormItem>

        <BaseFormItem label="存储桶" description="私有存储桶名称">
          <BaseInput :model-value="storage.bucket" :disabled="disabled" aria-label="存储桶" @update:model-value="updateStorage('bucket', $event)" />
        </BaseFormItem>

        <BaseFormItem label="对象键前缀" description="备份对象的存储路径前缀，不影响已有备份">
          <BaseInput :model-value="storage.prefix" :disabled="disabled" aria-label="对象键前缀" @update:model-value="updateStorage('prefix', $event)" />
        </BaseFormItem>

        <BaseFormItem label="Access Key ID" description="对象存储专用凭据">
          <BaseInput
            :model-value="storage.accessKeyId" :disabled="disabled"
            aria-label="Access Key ID"
            :type="accessKeyInputType"
            autocomplete="off"
            @update:model-value="updateStorage('accessKeyId', $event)"
          >
            <template #suffix>
              <BaseIconButton
                variant="ghost"
                size="sm"
                :label="accessKeyVisible ? '隐藏 Access Key ID' : '显示 Access Key ID'"
                @mousedown.prevent
                @click="toggleAccessKeyVisible"
              >
                <EyeOff v-if="accessKeyVisible" :size="16" />
                <Eye v-else :size="16" />
              </BaseIconButton>
            </template>
          </BaseInput>
        </BaseFormItem>

        <BaseFormItem label="Secret Access Key" description="对象存储专用 Secret">
          <BaseInput
            :model-value="storage.secretAccessKey" :disabled="disabled"
            aria-label="Secret Access Key"
            :type="secretInputType"
            autocomplete="new-password"
            @update:model-value="updateStorage('secretAccessKey', $event)"
          >
            <template #suffix>
              <BaseIconButton
                variant="ghost"
                size="sm"
                :label="secretVisible ? '隐藏 Secret Access Key' : '显示 Secret Access Key'"
                @mousedown.prevent
                @click="toggleSecretVisible"
              >
                <EyeOff v-if="secretVisible" :size="16" />
                <Eye v-else :size="16" />
              </BaseIconButton>
            </template>
          </BaseInput>
        </BaseFormItem>

        <div class="col-span-2 flex items-center gap-4 @max-[640px]:col-span-1">
          <BaseCheckbox :model-value="storage.forcePathStyle" :disabled="disabled" label="强制路径式访问" show-label @update:model-value="updateStorage('forcePathStyle', $event)" />
        </div>
      </BaseForm>
    </div>
  </BaseCard>
</template>
