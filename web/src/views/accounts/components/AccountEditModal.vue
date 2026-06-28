<script setup lang="ts">
import { Fingerprint } from '@lucide/vue'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import AccountStatusBadge from './AccountStatusBadge.vue'

const props = defineProps<{
  account: any
  saving: boolean
}>()

const open = defineModel<boolean>({ default: false })
const form = defineModel<any>('form', { required: true })
const status = defineModel<string>('status', { required: true })

const emit = defineEmits<{
  save: []
}>()

const accountEmail = computed(() => props.account?.email || '未绑定邮箱')
const accountPlan = computed(() => props.account?.planType || '-')

const detailItems = computed(() => [
  {
    label: 'ChatGPT 账号 ID',
    value: props.account?.accountId,
    itemClass: 'sm:col-span-2',
    valueClass: 'font-mono text-[12px]',
  },
  {
    label: '用户 ID',
    value: props.account?.userId,
    itemClass: '',
    valueClass: 'font-mono text-[12px]',
  },
  {
    label: '内部 ID',
    value: props.account?.id,
    itemClass: '',
    valueClass: 'font-mono text-[12px]',
  },
])

function formField(key: string) {
  return computed({
    get: () => form.value[key] ?? '',
    set: (value: string) => {
      form.value = { ...form.value, [key]: value }
    },
  })
}

const label = formField('label')

const scheduleEnabled = computed({
  get: () => status.value !== 'disabled',
  set: (enabled: boolean) => {
    status.value = enabled ? 'active' : 'disabled'
  },
})
</script>

<template>
  <BaseModal
    v-model="open"
    title="编辑账号"
    description="调整账号备注和调度状态。"
    width="680px"
    :close-disabled="saving"
  >
    <div class="space-y-6">
      <section class="rounded-(--cp-card-radius) bg-(--cp-bg-subtle) px-5 py-5">
        <div class="grid min-w-0 gap-4 sm:grid-cols-[44px_minmax(0,1fr)_auto] sm:items-center">
          <div
            class="hidden size-11 items-center justify-center rounded-[14px] bg-(--cp-info-bg) text-(--cp-info-text) sm:inline-flex"
          >
            <Fingerprint class="size-5" :stroke-width="2.2" />
          </div>
          <div class="min-w-0">
            <p class="m-0 text-[11px] leading-none font-[760] text-(--cp-info-text)">账号身份</p>
            <p
              class="mt-2 mb-0 truncate text-[16px] leading-tight font-[760] text-(--cp-text-primary)"
              :title="accountEmail"
            >
              {{ accountEmail }}
            </p>
            <p class="mt-2 mb-0 text-[12px] leading-none font-[650] text-(--cp-text-secondary)">
              套餐 {{ accountPlan }}
            </p>
          </div>
          <AccountStatusBadge :status="account?.status" variant="pill" />
        </div>

        <dl
          class="mt-5 grid gap-x-7 gap-y-3.5 pt-4 shadow-[inset_0_1px_0_var(--cp-divider-subtle)] sm:grid-cols-2"
        >
          <div
            v-for="item in detailItems"
            :key="item.label"
            class="min-w-0"
            :class="item.itemClass"
          >
            <dt class="text-[11px] leading-none font-[760] text-(--cp-text-muted)">
              {{ item.label }}
            </dt>
            <dd
              class="mt-2 mb-0 truncate text-[13px] leading-tight font-[650] text-(--cp-text-primary)"
              :class="item.valueClass"
              :title="item.value || '-'"
            >
              {{ item.value || '-' }}
            </dd>
          </div>
        </dl>
      </section>

      <BaseForm :columns="2">
        <BaseFormItem label="备注标签">
          <BaseInput v-model="label" placeholder="例如：主账号 / 备用账号" :disabled="saving" />
        </BaseFormItem>

        <BaseFormItem label="调度开关">
          <div class="grid box-content overflow-visible p-0.75">
            <div class="flex h-(--cp-input-height-default) items-center">
              <BaseSwitch v-model="scheduleEnabled" label="调度开关" :disabled="saving" />
            </div>
          </div>
        </BaseFormItem>
      </BaseForm>
    </div>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">取消</BaseButton>
      <BaseButton variant="primary" :loading="saving" @click="emit('save')">保存</BaseButton>
    </template>
  </BaseModal>
</template>
