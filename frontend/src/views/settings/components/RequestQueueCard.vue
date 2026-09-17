<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const maxWaitingPerKey = defineModel<string>('maxWaitingPerKey', { required: true })
const maxWaitingPerAccount = defineModel<string>('maxWaitingPerAccount', { required: true })
const concurrencyWaitTimeoutSeconds = defineModel<string>('concurrencyWaitTimeoutSeconds', { required: true })
</script>

<template>
  <BaseCard title="请求排队">
    <BaseForm class="max-w-6xl sm:grid-cols-2">
      <BaseFormItem label="每个密钥最大排队数" description="各密钥独立计数；0 表示不排队">
        <BaseInput v-model="maxWaitingPerKey" aria-label="每个密钥最大排队数" type="number" min="0" max="1000" step="1" />
      </BaseFormItem>
      <BaseFormItem label="单账号最大排队数" description="各账号独立计数；0 表示不排队">
        <BaseInput v-model="maxWaitingPerAccount" aria-label="单账号最大排队数" type="number" min="0" max="1000" step="1" />
      </BaseFormItem>
      <BaseFormItem label="最长排队秒数" description="密钥和账号共用，从首次入队起计时，1～120 秒">
        <BaseInput v-model="concurrencyWaitTimeoutSeconds" aria-label="最长排队秒数" type="number" min="1" max="120" step="1" />
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
