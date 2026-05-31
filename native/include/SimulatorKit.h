#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <IOSurface/IOSurfaceObjC.h>

@protocol SimDeviceIOProtocol <NSObject>
@property (nonatomic, readonly) NSArray *ioPorts;
@end

@protocol SimDeviceIOPortInterface <NSObject>
- (id)descriptor;
@end

@protocol SimDisplayRenderable <NSObject>
@end

@protocol SimScreenProperties <NSObject>
@end

@protocol SimScreen <NSObject>
- (void)registerScreenCallbacksWithUUID:(NSUUID *)uuid
                          callbackQueue:(dispatch_queue_t)queue
                          frameCallback:(void (^)(void))frameCallback
                surfacesChangedCallback:(void (^)(IOSurface * _Nullable framebufferSurface,
                                                  IOSurface * _Nullable maskedFramebufferSurface))callback
              propertiesChangedCallback:(void (^)(id<SimScreenProperties> _Nullable properties))callback;
- (void)unregisterScreenCallbacksWithUUID:(NSUUID *)uuid;
@end

@protocol SimDisplayDescriptorState <NSObject>
@property (nonatomic, readonly) unsigned short displayClass;
@end

@protocol SimDisplayIOSurfaceRenderable <NSObject>
@property (nonatomic, readonly, nullable) IOSurface *ioSurface;
@property (nonatomic, readonly, nullable) IOSurface *framebufferSurface;
@property (nonatomic, readonly, nullable) IOSurface *maskedFramebufferSurface;

- (void)registerCallbackWithUUID:(NSUUID *)uuid
         ioSurfaceChangeCallback:(void (^)(IOSurface * _Nullable surface))callback;
- (void)registerCallbackWithUUID:(NSUUID *)uuid
        ioSurfacesChangeCallback:(void (^)(IOSurface * _Nullable framebufferSurface,
                                          IOSurface * _Nullable maskedFramebufferSurface))callback;
- (void)registerCallbackWithUUID:(NSUUID *)uuid
        damageRectanglesCallback:(void (^)(NSArray<NSValue *> *rects))callback;

- (void)unregisterIOSurfaceChangeCallbackWithUUID:(NSUUID *)uuid;
- (void)unregisterIOSurfacesChangeCallbackWithUUID:(NSUUID *)uuid;
- (void)unregisterDamageRectanglesCallbackWithUUID:(NSUUID *)uuid;
@end

@interface SimDeviceLegacyClient : NSObject
- (nullable instancetype)initWithDevice:(id)device error:(NSError **)error;
- (void)sendWithMessage:(void *)message
           freeWhenDone:(BOOL)freeWhenDone
        completionQueue:(dispatch_queue_t)queue
             completion:(void (^)(NSError * _Nullable error))completion;
@end

#pragma pack(push, 4)
typedef struct {
  double field1;
  double field2;
  double field3;
  double field4;
} IndigoQuad;

typedef struct {
  unsigned int field1;
  unsigned int field2;
  unsigned int field3;
  double xRatio;
  double yRatio;
  double field6;
  double field7;
  double field8;
  unsigned int field9;
  unsigned int field10;
  unsigned int field11;
  unsigned int field12;
  unsigned int field13;
  double field14;
  double field15;
  double field16;
  double field17;
  double field18;
} IndigoTouch;

typedef struct {
  unsigned int eventSource;
  unsigned int eventType;
  unsigned int eventTarget;
  unsigned int keyCode;
  unsigned int field5;
} IndigoButton;

typedef struct {
  unsigned int field1;
  double field2;
  double field3;
  double field4;
  unsigned int field5;
} IndigoWheel;

typedef struct {
  unsigned int field1;
  unsigned char field2[40];
} IndigoAccelerometer;

typedef struct {
  unsigned int field1;
  double field2;
  unsigned int field3;
  double field4;
} IndigoForce;

typedef struct {
  IndigoQuad dpad;
  IndigoQuad face;
  IndigoQuad shoulder;
  IndigoQuad joystick;
} IndigoGameController;

typedef union {
  IndigoTouch touch;
  IndigoWheel wheel;
  IndigoButton button;
  IndigoAccelerometer accelerometer;
  IndigoForce force;
  IndigoGameController gameController;
} IndigoEvent;

typedef struct {
  unsigned int field1;
  unsigned long long timestamp;
  unsigned int field3;
  IndigoEvent event;
} IndigoPayload;

typedef struct {
  unsigned int msgh_bits;
  unsigned int msgh_size;
  unsigned int msgh_remote_port;
  unsigned int msgh_local_port;
  unsigned int msgh_voucher_port;
  int msgh_id;
} MachMessageHeader;

typedef struct {
  MachMessageHeader header;
  unsigned int innerSize;
  unsigned char eventType;
  unsigned char reserved[3];
  IndigoPayload payload;
} IndigoMessage;
#pragma pack(pop)
