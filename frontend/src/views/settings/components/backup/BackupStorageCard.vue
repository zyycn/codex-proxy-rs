<script setup lang="ts">
import { CircleAlert, CircleCheck, DatabaseZap, Eye, EyeOff, Save } from '@lucide/vue'
import { computed, shallowRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

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
  loading: boolean
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
</script>

<template>
  <BaseCard
    :padded="false"
    title="S3 存储配置"
    header-class="px-5 pt-4"
    body-class="px-5 py-5"
  >
    <template #description>
      <span class="text-(--cp-text-secondary)">
        配置 S3 兼容存储（支持
        <button
          type="button"
          class="cursor-pointer border-0 bg-transparent p-0 text-(--cp-info) underline underline-offset-2"
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
          variant="default"
          :loading="testing"
          :disabled="loading"
          :title="verified ? '已通过连接测试' : '尚未通过连接测试'"
          @click="emit('test')"
        >
          <template #icon>
            <CircleCheck
              v-if="verified"
              class="size-4 text-(--cp-success-text)"
              aria-hidden="true"
            />
            <CircleAlert
              v-else
              class="size-4 text-(--cp-warning-text)"
              aria-hidden="true"
            />
          </template>
          {{ testing ? '测试中...' : '测试连接' }}
        </BaseButton>
        <BaseButton variant="primary" :loading="saving" :disabled="loading" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存' }}
        </BaseButton>
      </div>
    </template>

    <div class="@container">
      <BaseForm :columns="2" class="max-w-6xl @max-[640px]:grid-cols-1!">
        <BaseFormItem label="端点地址" description="S3 兼容服务的 HTTPS 地址">
          <BaseInput
            v-model="storage.endpoint"
            aria-label="端点地址"
            placeholder="https://<account_id>.r2.cloudflarestorage.com"
          >
            <template #prefix>
              <DatabaseZap class="size-4" />
            </template>
          </BaseInput>
        </BaseFormItem>

        <BaseFormItem label="区域" description="R2 使用固定值 auto；其它服务按提供方填写">
          <BaseInput v-model="storage.region" aria-label="区域" />
        </BaseFormItem>

        <BaseFormItem label="存储桶" description="私有存储桶名称">
          <BaseInput v-model="storage.bucket" aria-label="存储桶" />
        </BaseFormItem>

        <BaseFormItem label="Key 前缀" description="对象前缀，历史归档已保存完整旧前缀">
          <BaseInput v-model="storage.prefix" aria-label="Key 前缀" />
        </BaseFormItem>

        <BaseFormItem label="Access Key ID" description="对象存储专用凭据">
          <BaseInput
            v-model="storage.accessKeyId"
            aria-label="Access Key ID"
            :type="accessKeyInputType"
            autocomplete="off"
          >
            <template #suffix>
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                :label="accessKeyVisible ? '隐藏 Access Key ID' : '显示 Access Key ID'"
                @mousedown.prevent
                @click="toggleAccessKeyVisible"
              >
                <EyeOff v-if="accessKeyVisible" :size="16" />
                <Eye v-else :size="16" />
              </BaseButton>
            </template>
          </BaseInput>
        </BaseFormItem>

        <BaseFormItem label="Secret Access Key" description="对象存储专用 Secret">
          <BaseInput
            v-model="storage.secretAccessKey"
            aria-label="Secret Access Key"
            :type="secretInputType"
            autocomplete="new-password"
          >
            <template #suffix>
              <BaseButton
                icon-only
                variant="ghost"
                size="sm"
                :label="secretVisible ? '隐藏 Secret Access Key' : '显示 Secret Access Key'"
                @mousedown.prevent
                @click="toggleSecretVisible"
              >
                <EyeOff v-if="secretVisible" :size="16" />
                <Eye v-else :size="16" />
              </BaseButton>
            </template>
          </BaseInput>
        </BaseFormItem>

        <div class="col-span-2 flex items-center gap-4 @max-[640px]:col-span-1">
          <BaseCheckbox v-model="storage.forcePathStyle" label="强制路径风格" show-label />
        </div>
      </BaseForm>
    </div>
  </BaseCard>
</template>
