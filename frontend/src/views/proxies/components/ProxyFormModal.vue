<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { Eye, EyeOff, Save, Wifi } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
  saving: boolean
  testing: boolean
}>()
const emit = defineEmits<{
  save: []
  test: []
}>()
const open = defineModel<boolean>({ required: true })
const name = defineModel<string>('name', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const showSecret = shallowRef(false)
const busy = computed(() => props.saving || props.testing)
const title = computed(() => props.proxy ? '编辑代理' : '新增代理')
const connectionDescription = computed(() => props.proxy
  ? '留空保留当前连接和认证信息；填写新地址时，请包含所需的用户名和密码。'
  : '支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H，可在地址中包含用户名和密码。')

watch(open, () => {
  showSecret.value = false
})
</script>

<template>
  <BaseModal v-model="open" :title="title" size="md" :dismissible="!busy">
    <BaseForm class="grid gap-5">
      <BaseFormItem label="代理名称" required>
        <BaseInput v-model="name" maxlength="100" :disabled="busy" aria-label="代理名称" placeholder="请输入代理名称" />
      </BaseFormItem>
      <BaseFormItem label="代理 URL" :required="!proxy" :description="connectionDescription">
        <BaseInput
          v-model="proxyUrl"
          :type="showSecret ? 'text' : 'password'"
          autocomplete="new-password"
          :disabled="busy"
          aria-label="代理 URL"
          placeholder="请输入代理 URL"
        >
          <template #suffix>
            <BaseIconButton :label="showSecret ? '隐藏代理地址' : '显示代理地址'" :disabled="busy" @click="showSecret = !showSecret">
              <EyeOff v-if="showSecret" class="size-4" />
              <Eye v-else class="size-4" />
            </BaseIconButton>
          </template>
        </BaseInput>
      </BaseFormItem>
      <p v-if="proxy?.accountCount && proxyUrl.trim()" class="m-0 text-cp-sm text-cp-warning-text">
        将更新 {{ proxy.accountCount }} 个关联账号的出口。
      </p>
    </BaseForm>
    <template #footer>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="secondary" :loading="testing" :disabled="saving" @click="emit('test')">
        <template #icon>
          <Wifi class="size-4" />
        </template>
        测试连接
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="testing" @click="emit('save')">
        <template #icon>
          <Save class="size-4" />
        </template>
        保存代理
      </BaseButton>
    </template>
  </BaseModal>
</template>
