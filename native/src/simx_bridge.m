#import "CoreSimulator.h"
#import "SimulatorKit.h"

#import <CoreImage/CoreImage.h>
#import <ImageIO/ImageIO.h>
#import <IOSurface/IOSurface.h>
#import <dlfcn.h>
#import <mach/mach_time.h>
#import <objc/runtime.h>

typedef void (*SimxFrameCallback)(const unsigned char *bytes, unsigned long length, void *context);
typedef IndigoMessage *(*SimxMouseMessageFn)(CGPoint *location, CGPoint *windowLocation, uint32_t target, NSInteger eventType, CGSize displaySize, uint32_t edge);
typedef IndigoMessage *(*SimxKeyboardMessageFn)(int keyCode, int operation);
typedef IndigoMessage *(*SimxButtonMessageFn)(uint32_t buttonCode, uint32_t operation, uint32_t target);
typedef IndigoMessage *(*SimxArbitraryHIDMessageFn)(uint32_t target, uint32_t page, uint32_t usage, uint32_t operation);

static uint32_t const SimxTouchTarget = 0x32;
static int const SimxKeyboardDown = 1;
static int const SimxKeyboardUp = 2;
static uint32_t const SimxButtonDown = 1;
static uint32_t const SimxButtonUp = 2;
static uint32_t const SimxButtonTargetHardware = 0x2;
static uint32_t const SimxConsumerControlUsagePage = 0x0c;
static uint32_t const SimxHomeMenuUsage = 0x40;
static uint32_t const SimxHomeUsage = 0x65;
static uint32_t const SimxHomeButtonCode = 0x191;

typedef struct {
    BOOL useButtonMessage;
    uint32_t buttonCode;
    uint32_t target;
    uint32_t page;
    uint32_t usage;
} SimxHomeStrategy;

static void simx_set_error(char **error, NSString *message);

@interface SimxFrameStreamer : NSObject
@property (nonatomic, strong) SimDevice *device;
@property (nonatomic, strong) id<SimDisplayIOSurfaceRenderable> surface;
@property (nonatomic, strong) id<SimScreen> screen;
@property (nonatomic, strong) NSUUID *uuid;
@property (nonatomic, strong) CIContext *ciContext;
@property (nonatomic, strong) dispatch_queue_t encodeQueue;
@property (nonatomic, assign) uint32_t lastSeed;
@property (nonatomic, assign) float quality;
@property (nonatomic, assign) SimxFrameCallback callback;
@property (nonatomic, assign) void *callbackContext;
@property (nonatomic, assign) BOOL stopped;
@property (nonatomic, strong) id hidClient;
@property (nonatomic, assign) SimxMouseMessageFn mouseMessage;
@property (nonatomic, assign) SimxKeyboardMessageFn keyboardMessage;
@property (nonatomic, assign) SimxButtonMessageFn buttonMessage;
@property (nonatomic, assign) SimxArbitraryHIDMessageFn arbitraryHIDMessage;
@end

@implementation SimxFrameStreamer

- (instancetype)initWithDevice:(SimDevice *)device
                       surface:(id<SimDisplayIOSurfaceRenderable>)surface
                        screen:(id<SimScreen>)screen
                          uuid:(NSUUID *)uuid
                       quality:(float)quality
                      callback:(SimxFrameCallback)callback
               callbackContext:(void *)callbackContext
{
    self = [super init];
    if (!self) { return nil; }
    _device = device;
    _surface = surface;
    _screen = screen;
    _uuid = uuid;
    _quality = quality;
    _callback = callback;
    _callbackContext = callbackContext;
    _ciContext = [CIContext contextWithOptions:nil];
    _encodeQueue = dispatch_queue_create("simx.frame.encode", DISPATCH_QUEUE_SERIAL);
    return self;
}

- (void)handleSurface:(IOSurface *)surface
{
    if (self.stopped || surface == nil || self.callback == NULL) { return; }
    IOSurface *retainedSurface = surface;
    dispatch_async(self.encodeQueue, ^{
        @try {
            if (self.stopped) {
                return;
            }
            IOSurfaceRef surfaceRef = (__bridge IOSurfaceRef)retainedSurface;
            IOSurfaceIncrementUseCount(surfaceRef);
            uint32_t seed = IOSurfaceGetSeed(surfaceRef);
            if (seed == self.lastSeed) {
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            self.lastSeed = seed;
            CIImage *ciImage = [CIImage imageWithIOSurface:surfaceRef];
            if (ciImage == nil) {
                IOSurfaceDecrementUseCount(surfaceRef);
                return;
            }
            CGImageRef image = [self.ciContext createCGImage:ciImage fromRect:[ciImage extent]];
            IOSurfaceDecrementUseCount(surfaceRef);
            if (image == NULL) { return; }

            NSMutableData *data = [NSMutableData data];
            CGImageDestinationRef destination = CGImageDestinationCreateWithData((__bridge CFMutableDataRef)data, CFSTR("public.jpeg"), 1, NULL);
            if (destination == NULL) {
                CGImageRelease(image);
                return;
            }
            NSDictionary *options = @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality: @(self.quality)};
            CGImageDestinationAddImage(destination, image, (__bridge CFDictionaryRef)options);
            BOOL ok = CGImageDestinationFinalize(destination);
            CFRelease(destination);
            CGImageRelease(image);
            if (!ok || data.length == 0 || self.stopped) { return; }
            self.callback((const unsigned char *)data.bytes, (unsigned long)data.length, self.callbackContext);
        } @catch (NSException *exception) {
            NSLog(@"simx frame bridge exception: %@", exception);
        }
    });
}

- (void)stop
{
    if (self.stopped) { return; }
    self.stopped = YES;
    id surfaceObject = self.surface;
    id screenObject = self.screen;
    if (screenObject != nil && [screenObject respondsToSelector:@selector(unregisterScreenCallbacksWithUUID:)]) {
        [screenObject unregisterScreenCallbacksWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterIOSurfaceChangeCallbackWithUUID:)]) {
        [surfaceObject unregisterIOSurfaceChangeCallbackWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterIOSurfacesChangeCallbackWithUUID:)]) {
        [surfaceObject unregisterIOSurfacesChangeCallbackWithUUID:self.uuid];
    }
    if ([surfaceObject respondsToSelector:@selector(unregisterDamageRectanglesCallbackWithUUID:)]) {
        [surfaceObject unregisterDamageRectanglesCallbackWithUUID:self.uuid];
    }
}

- (void)dealloc
{
    [self stop];
}

@end

@implementation SimxFrameStreamer (HID)

- (BOOL)sendMessage:(void *)message error:(char **)error
{
    if (message == NULL || self.hidClient == nil) {
        simx_set_error(error, @"SimulatorKit HID transport is unavailable.");
        return NO;
    }
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block NSError *sendError = nil;
    @try {
        [self.hidClient sendWithMessage:message
                           freeWhenDone:YES
                        completionQueue:dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0)
                             completion:^(NSError *completionError) {
            sendError = completionError;
            dispatch_semaphore_signal(semaphore);
        }];
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit HID exception: %@", exception.reason ?: exception.name]);
        free(message);
        return NO;
    }
    dispatch_time_t deadline = dispatch_time(DISPATCH_TIME_NOW, (int64_t)(2 * NSEC_PER_SEC));
    if (dispatch_semaphore_wait(semaphore, deadline) != 0) {
        simx_set_error(error, @"Timed out waiting for SimulatorKit HID delivery.");
        return NO;
    }
    if (sendError != nil) {
        simx_set_error(error, sendError.localizedDescription);
        return NO;
    }
    return YES;
}

- (BOOL)sendTouchX:(double)nx y:(double)ny down:(BOOL)down error:(char **)error
{
    if (self.mouseMessage == NULL) {
        simx_set_error(error, @"SimulatorKit did not expose IndigoHIDMessageForMouseNSEvent.");
        return NO;
    }
    nx = fmax(0.0, fmin(1.0, nx));
    ny = fmax(0.0, fmin(1.0, ny));
    CGPoint point = CGPointMake(nx, ny);
    CGSize displaySize = self.device.deviceType.mainScreenSize;
    NSInteger mouseEventType = down ? 1 : 2;
    IndigoMessage *baseMessage = self.mouseMessage(&point, NULL, SimxTouchTarget, mouseEventType, displaySize, 0);
    if (baseMessage == NULL) {
        simx_set_error(error, @"SimulatorKit failed to create the base touch HID packet.");
        return NO;
    }
    size_t messageSize = sizeof(IndigoMessage) + sizeof(IndigoPayload);
    IndigoMessage *message = calloc(1, messageSize);
    if (message == NULL) {
        free(baseMessage);
        simx_set_error(error, @"Unable to allocate touch HID packet.");
        return NO;
    }
    message->innerSize = (uint32_t)sizeof(IndigoPayload);
    message->eventType = 0x02;
    message->payload.field1 = 0x0000000b;
    message->payload.timestamp = mach_absolute_time();
    message->payload.event.touch = baseMessage->payload.event.touch;
    message->payload.event.touch.xRatio = nx;
    message->payload.event.touch.yRatio = ny;
    IndigoPayload *second = (IndigoPayload *)(((uint8_t *)&message->payload) + sizeof(IndigoPayload));
    memcpy(second, &message->payload, sizeof(IndigoPayload));
    second->event.touch.field1 = 0x00000001;
    second->event.touch.field2 = 0x00000002;
    free(baseMessage);
    return [self sendMessage:message error:error];
}

- (BOOL)sendKeyCode:(uint16_t)keyCode down:(BOOL)down error:(char **)error
{
    if (self.keyboardMessage == NULL) {
        simx_set_error(error, @"SimulatorKit did not expose IndigoHIDMessageForKeyboardArbitrary.");
        return NO;
    }
    IndigoMessage *message = self.keyboardMessage((int)keyCode, down ? SimxKeyboardDown : SimxKeyboardUp);
    if (message == NULL) {
        simx_set_error(error, @"SimulatorKit failed to create keyboard HID packet.");
        return NO;
    }
    return [self sendMessage:message error:error];
}

- (BOOL)pressHome:(char **)error
{
    static const SimxHomeStrategy strategies[] = {
        { NO, 0, SimxTouchTarget, SimxConsumerControlUsagePage, SimxHomeMenuUsage },
        { NO, 0, SimxTouchTarget, SimxConsumerControlUsagePage, SimxHomeUsage },
        { YES, SimxHomeButtonCode, SimxButtonTargetHardware, 0, 0 },
        { YES, SimxHomeButtonCode, SimxTouchTarget, 0, 0 },
    };
    NSString *lastError = nil;
    for (size_t index = 0; index < sizeof(strategies) / sizeof(strategies[0]); index++) {
        const SimxHomeStrategy *strategy = &strategies[index];
        IndigoMessage *down = NULL;
        IndigoMessage *up = NULL;
        if (strategy->useButtonMessage) {
            if (self.buttonMessage == NULL) {
                lastError = @"SimulatorKit did not expose IndigoHIDMessageForButton.";
                continue;
            }
            down = self.buttonMessage(strategy->buttonCode, SimxButtonDown, strategy->target);
            up = self.buttonMessage(strategy->buttonCode, SimxButtonUp, strategy->target);
        } else {
            if (self.arbitraryHIDMessage == NULL) {
                lastError = @"SimulatorKit did not expose IndigoHIDMessageForHIDArbitrary.";
                continue;
            }
            down = self.arbitraryHIDMessage(strategy->target, strategy->page, strategy->usage, SimxButtonDown);
            up = self.arbitraryHIDMessage(strategy->target, strategy->page, strategy->usage, SimxButtonUp);
        }
        if (down == NULL || up == NULL) {
            if (down != NULL) { free(down); }
            if (up != NULL) { free(up); }
            lastError = @"SimulatorKit could not create Home HID packets.";
            continue;
        }
        char *downError = NULL;
        BOOL downOK = [self sendMessage:down error:&downError];
        if (!downOK) {
            lastError = downError != NULL ? [NSString stringWithUTF8String:downError] : @"Home button down failed.";
            if (downError != NULL) { free(downError); }
            free(up);
            continue;
        }
        [NSThread sleepForTimeInterval:0.08];
        char *upError = NULL;
        BOOL upOK = [self sendMessage:up error:&upError];
        if (upOK) {
            return YES;
        }
        lastError = upError != NULL ? [NSString stringWithUTF8String:upError] : @"Home button up failed.";
        if (upError != NULL) { free(upError); }
    }
    simx_set_error(error, lastError ?: @"SimulatorKit rejected every Home HID strategy.");
    return NO;
}

@end

static char *simx_strdup(NSString *message) {
    if (message == nil) { message = @"Unknown native SimStream bridge error."; }
    const char *utf8 = message.UTF8String;
    if (utf8 == NULL) { utf8 = "Unknown native SimStream bridge error."; }
    return strdup(utf8);
}

static void simx_set_error(char **error, NSString *message) {
    if (error != NULL) { *error = simx_strdup(message); }
}

void simx_bridge_free_string(char *value) {
    if (value != NULL) { free(value); }
}

void *simx_frame_stream_start(const char *developer_dir,
                              const char *udid,
                              float quality,
                              SimxFrameCallback callback,
                              void *callback_context,
                              char **error)
{
    @autoreleasepool {
        if (callback == NULL) {
            simx_set_error(error, @"Frame callback was NULL.");
            return NULL;
        }

        NSString *devDir = developer_dir != NULL ? [NSString stringWithUTF8String:developer_dir] : nil;
        NSString *targetUDID = udid != NULL ? [NSString stringWithUTF8String:udid] : nil;
        if (devDir.length == 0 || targetUDID.length == 0) {
            simx_set_error(error, @"Developer dir and simulator UDID are required.");
            return NULL;
        }

        NSString *coreSimulatorPath = @"/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator";
        void *coreHandle = dlopen(coreSimulatorPath.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        if (coreHandle == NULL) {
            simx_set_error(error, [NSString stringWithFormat:@"Could not load CoreSimulator: %s", dlerror()]);
            return NULL;
        }

        NSString *simulatorKitPath = [devDir stringByAppendingPathComponent:@"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"];
        void *simKitHandle = dlopen(simulatorKitPath.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        if (simKitHandle == NULL) {
            simx_set_error(error, [NSString stringWithFormat:@"Could not load SimulatorKit: %s", dlerror()]);
            return NULL;
        }

        Class contextClass = NSClassFromString(@"SimServiceContext");
        if (contextClass == Nil) {
            simx_set_error(error, @"CoreSimulator did not expose SimServiceContext.");
            return NULL;
        }
        NSError *contextError = nil;
        id context = [contextClass sharedServiceContextForDeveloperDir:devDir error:&contextError];
        if (context == nil) {
            simx_set_error(error, contextError.localizedDescription ?: @"CoreSimulator service context failed.");
            return NULL;
        }
        NSError *deviceSetError = nil;
        SimDeviceSet *deviceSet = [context defaultDeviceSetWithError:&deviceSetError];
        if (deviceSet == nil) {
            simx_set_error(error, deviceSetError.localizedDescription ?: @"Could not load default simulator device set.");
            return NULL;
        }

        SimDevice *target = nil;
        for (SimDevice *device in deviceSet.availableDevices) {
            if ([[device.UDID UUIDString] caseInsensitiveCompare:targetUDID] == NSOrderedSame) {
                target = device;
                break;
            }
        }
        if (target == nil) {
            simx_set_error(error, [NSString stringWithFormat:@"Simulator %@ was not found.", targetUDID]);
            return NULL;
        }
        if (target.state != 3) {
            simx_set_error(error, [NSString stringWithFormat:@"Simulator %@ is not booted.", targetUDID]);
            return NULL;
        }
        id hidClient = nil;
        Class clientClass = objc_lookUpClass("SimulatorKit.SimDeviceLegacyHIDClient");
        if (clientClass != Nil) {
            NSError *hidError = nil;
            @try {
                hidClient = [[clientClass alloc] initWithDevice:target error:&hidError];
            } @catch (NSException *exception) {
                NSLog(@"simx HID client init exception: %@", exception);
                hidClient = nil;
            }
            (void)hidError;
        }
        id<SimDeviceIOProtocol> io = target.io;
        if (io == nil) {
            simx_set_error(error, @"Booted simulator did not expose IO ports.");
            return NULL;
        }

        id<SimDisplayIOSurfaceRenderable> mainSurface = nil;
        id<SimScreen> mainScreen = nil;
        for (id port in io.ioPorts) {
            if (![port conformsToProtocol:@protocol(SimDeviceIOPortInterface)]) { continue; }
            id descriptor = [(id<SimDeviceIOPortInterface>)port descriptor];
            if (![descriptor conformsToProtocol:@protocol(SimDisplayRenderable)] ||
                ![descriptor conformsToProtocol:@protocol(SimDisplayIOSurfaceRenderable)]) {
                continue;
            }
            if ([descriptor respondsToSelector:@selector(state)]) {
                id state = [descriptor performSelector:@selector(state)];
                if ([state respondsToSelector:@selector(displayClass)] &&
                    [(id<SimDisplayDescriptorState>)state displayClass] != 0) {
                    continue;
                }
            }
            mainSurface = (id<SimDisplayIOSurfaceRenderable>)descriptor;
            if ([descriptor conformsToProtocol:@protocol(SimScreen)]) {
                mainScreen = (id<SimScreen>)descriptor;
            }
            break;
        }
        if (mainSurface == nil) {
            simx_set_error(error, @"Could not find main IOSurface display.");
            return NULL;
        }

        NSUUID *uuid = [NSUUID UUID];
        dispatch_queue_t callbackQueue = dispatch_queue_create("simx.frame.callbacks", DISPATCH_QUEUE_SERIAL);
        SimxFrameStreamer *streamer = [[SimxFrameStreamer alloc] initWithDevice:target
                                                                        surface:mainSurface
                                                                         screen:mainScreen
                                                                           uuid:uuid
                                                                        quality:fmaxf(0.0f, fminf(1.0f, quality))
                                                                       callback:callback
                                                                callbackContext:callback_context];
        streamer.hidClient = hidClient;
        streamer.mouseMessage = (SimxMouseMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForMouseNSEvent");
        streamer.keyboardMessage = (SimxKeyboardMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForKeyboardArbitrary");
        streamer.buttonMessage = (SimxButtonMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForButton");
        streamer.arbitraryHIDMessage = (SimxArbitraryHIDMessageFn)dlsym(simKitHandle, "IndigoHIDMessageForHIDArbitrary");

        __weak SimxFrameStreamer *weakStreamer = streamer;
        id surfaceObject = mainSurface;
        if (mainScreen != nil && [mainScreen respondsToSelector:@selector(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:)]) {
            [mainScreen registerScreenCallbacksWithUUID:uuid
                                          callbackQueue:callbackQueue
                                          frameCallback:^{
                SimxFrameStreamer *strong = weakStreamer;
                IOSurface *surface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
                [strong handleSurface:surface];
            } surfacesChangedCallback:^(IOSurface *framebufferSurface, IOSurface *maskedFramebufferSurface) {
                SimxFrameStreamer *strong = weakStreamer;
                [strong handleSurface:(framebufferSurface ?: maskedFramebufferSurface)];
            } propertiesChangedCallback:^(id<SimScreenProperties> properties) {
                (void)properties;
            }];
        } else {
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:ioSurfaceChangeCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid ioSurfaceChangeCallback:^(IOSurface *surface) {
                    SimxFrameStreamer *strong = weakStreamer;
                    [strong handleSurface:surface];
                }];
            }
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:ioSurfacesChangeCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid ioSurfacesChangeCallback:^(IOSurface *framebufferSurface, IOSurface *maskedFramebufferSurface) {
                    SimxFrameStreamer *strong = weakStreamer;
                    [strong handleSurface:(framebufferSurface ?: maskedFramebufferSurface)];
                }];
            }
            if ([surfaceObject respondsToSelector:@selector(registerCallbackWithUUID:damageRectanglesCallback:)]) {
                [mainSurface registerCallbackWithUUID:uuid damageRectanglesCallback:^(NSArray<NSValue *> *rects) {
                    (void)rects;
                    SimxFrameStreamer *strong = weakStreamer;
                    IOSurface *surface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
                    [strong handleSurface:surface];
                }];
            }
        }

        IOSurface *initialSurface = mainSurface.framebufferSurface ?: mainSurface.maskedFramebufferSurface ?: mainSurface.ioSurface;
        [streamer handleSurface:initialSurface];

        return (__bridge_retained void *)streamer;
    }
}

void simx_frame_stream_stop(void *handle)
{
    if (handle == NULL) { return; }
    @autoreleasepool {
        SimxFrameStreamer *streamer = (__bridge_transfer SimxFrameStreamer *)handle;
        [streamer stop];
    }
}

int simx_hid_touch(void *handle, double nx, double ny, int down, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer sendTouchX:nx y:ny down:(down != 0) error:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit touch exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}

int simx_hid_key(void *handle, unsigned short keyCode, int down, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer sendKeyCode:keyCode down:(down != 0) error:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit key exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}

int simx_hid_home(void *handle, char **error)
{
    if (handle == NULL) {
        simx_set_error(error, @"Native stream handle was NULL.");
        return 0;
    }
    @try {
        SimxFrameStreamer *streamer = (__bridge SimxFrameStreamer *)handle;
        return [streamer pressHome:error] ? 1 : 0;
    } @catch (NSException *exception) {
        simx_set_error(error, [NSString stringWithFormat:@"SimulatorKit Home exception: %@", exception.reason ?: exception.name]);
        return 0;
    }
}
