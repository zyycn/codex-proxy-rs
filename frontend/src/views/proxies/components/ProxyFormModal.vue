<script setup lang="ts">
import type { ProxyFormValue } from '../composables/useProxies'
import type { OutboundProxy, OutboundProxyTestResult } from '@/api'
import { computed } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'

const props = defineProps<{
  proxy: OutboundProxy | null
  saving: boolean
  testing: boolean
  testResult: OutboundProxyTestResult | null
}>()
const emit = defineEmits<{
  save: []
  test: []
}>()
const open = defineModel<boolean>({ required: true })
const form = defineModel<ProxyFormValue>('form', { required: true })
const title = computed(() => props.proxy ? '编辑代理' : '添加代理')
const description = computed(() => props.proxy
  ? '修改代理名称或替换代理地址；地址留空则保持不变。'
  : '添加后可在账号编辑中把该代理设为账号的出站隧道。')
const busy = computed(() => props.saving || props.testing)
const canTest = computed(() => Boolean(form.value.url.trim() || props.proxy))
const canSave = computed(() =>
  Boolean(form.value.name.trim()) && (Boolean(props.proxy) || Boolean(form.value.url.trim())),
)
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    :description="description"
    size="md"
    :dismissible="!busy"
  >
    <BaseForm class="grid gap-5">
      <BaseFormItem label="代理名称" required>
        <BaseInput
          v-model="form.name"
          aria-label="代理名称"
          placeholder="例如：香港节点 01"
          :disabled="busy"
        />
      </BaseFormItem>
      <BaseFormItem label="代理 URL" :required="!proxy">
        <BaseInput
          v-model="form.url"
          type="password"
          autocomplete="new-password"
          aria-label="代理 URL"
          :placeholder="proxy ? '留空保持当前地址不变' : '例如：http://user:pass@1.2.3.4:7890'"
          :disabled="busy"
        />
        <p v-if="proxy" class="mt-2 mb-0 font-mono text-xs break-all text-cp-text-quaternary">
          当前地址：{{ proxy.endpoint }}
        </p>
        <p class="mt-2 mb-0 text-xs text-cp-text-quaternary">
          支持 http、https、socks5、socks5h，可包含用户名和密码。
        </p>
      </BaseFormItem>
      <BaseFormItem label="测试链接（可选）">
        <BaseInput
          v-model="form.targetUrl"
          aria-label="测试链接"
          placeholder="留空使用默认目标 https://api.openai.com/v1/models"
          :disabled="busy"
        />
      </BaseFormItem>

      <div
        v-if="testResult"
        class="rounded-cp px-4 py-3 text-cp-sm"
        :class="testResult.success
          ? 'bg-cp-success-container text-cp-success-on-container'
          : 'bg-cp-error-container text-cp-error-on-container'"
      >
        <template v-if="testResult.success">
          连通正常：{{ testResult.latencyMs }}ms
          <template v-if="testResult.statusCode">
            （HTTP {{ testResult.statusCode }}）
          </template>
        </template>
        <template v-else>
          测试失败：{{ testResult.error ?? '未知错误' }}
        </template>
        <p class="mt-1 mb-0 font-mono text-xs break-all opacity-75">
          目标：{{ testResult.targetUrl }}
        </p>
      </div>
    </BaseForm>

    <template #footer>
      <BaseButton
        variant="secondary"
        :loading="testing"
        :disabled="saving || !canTest"
        @click="emit('test')"
      >
        测试连通性
      </BaseButton>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="testing || !canSave"
        @click="emit('save')"
      >
        保存代理
      </BaseButton>
    </template>
  </BaseModal>
</template>
