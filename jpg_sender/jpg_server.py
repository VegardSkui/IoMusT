import simplejpeg
import cv2
import imagezmq

image_hub = imagezmq.ImageHub()

while True:
    client_name, jpg_frame = image_hub.recv_jpg()
    frame = simplejpeg.decode_jpeg(jpg_frame, colorspace="BGR")
    cv2.imshow(client_name, frame)
    cv2.waitKey(1)
    image_hub.send_reply(b"ok")