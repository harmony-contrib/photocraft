import { DefaultXComponent } from '@ohos-rs/ability';

@Entry
@Component
struct Index {
  build() {
    Stack() {
      DefaultXComponent({ moduleName: 'photocraft_ohos' })
    }
    .width('100%')
    .height('100%')
  }
}
