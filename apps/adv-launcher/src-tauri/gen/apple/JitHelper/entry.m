// Register the Rust request handler before Foundation starts the extension.
#import <Foundation/Foundation.h>
#import <objc/runtime.h>

extern void adv_jit_helper_init(void);

__attribute__((constructor))
static void register_jit_handler(void) {
    adv_jit_helper_init();
    // ExtensionFoundation calls the handler only if it adopts this protocol,
    // and objc2 adopts a protocol only once some image has loaded it.
    class_addProtocol(objc_getClass("HitodeJitRequestHandler"), @protocol(NSExtensionRequestHandling));
}
