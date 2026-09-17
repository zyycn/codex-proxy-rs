<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const model = defineModel<string>({ required: true })
</script>

<template>
  <BaseCard title="请求体解压上限">
    <BaseForm class="max-w-6xl sm:grid-cols-2 sm:items-end">
      <BaseFormItem label="最大解压大小" description="调高可接收更大的图片和历史上下文，也会增加内存占用">
        <BaseInput v-model="model" aria-label="请求体解压上限" type="number" min="1" step="1">
          <template #suffix>
            MiB
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
  </BaseCard>
</template>
